//! The Quarto dialect of the Markdown renderer.
//!
//! A `.qmd` is always kept as source. What the preview shows is a *draft*: the
//! source turned into plain Markdown that comrak renders, with a line map that
//! says which source line each draft line came from. The draft is rough on
//! purpose -- Quarto's own render is authoritative -- and it never runs code:
//!
//! - the front matter becomes a title block (title, authors, date, abstract);
//! - an executable cell is shown verbatim, or, when the host passed the output
//!   of a run, as its code followed by that output;
//! - top-level fenced divs and shortcodes this renderer does not understand
//!   are shown verbatim, so no half of them is rendered as Markdown;
//! - `{{< include >}}` is expanded from the files the host handed over;
//! - `@fig-x`, `@tbl-x`, `@sec-x`, `@eq-x` and `@lst-x` become numbered links
//!   when the target exists, and stay text when it does not.
//!
//! This is a port of the browser's former JavaScript draft, so that preview,
//! block rendering and publishing share one implementation.

use std::collections::HashMap;

use crate::diagnostic::{Diagnostic, Severity};
use crate::page::escape;

/// How deep includes may nest before the draft calls it a cycle.
const MAX_INCLUDE_DEPTH: usize = 8;

/// Says whether a filename is a Quarto document.
pub fn is_quarto(name: &str) -> bool {
    name.to_lowercase().ends_with(".qmd")
}

// ---------------------------------------------------------------- YAML

/// The small subset of YAML a front matter or a `#|` option uses.
#[derive(Clone, Debug, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

type Map = Vec<(String, Yaml)>;

impl Yaml {
    /// What JavaScript's `String(value)` made of it.
    fn text(&self) -> String {
        match self {
            Yaml::Null => String::new(),
            Yaml::Bool(b) => b.to_string(),
            Yaml::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Yaml::Str(s) => s.clone(),
            Yaml::List(items) => items.iter().map(Yaml::text).collect::<Vec<_>>().join(","),
            Yaml::Map(_) => "[object Object]".to_string(),
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Yaml::Null => false,
            Yaml::Bool(b) => *b,
            Yaml::Num(n) => *n != 0.0,
            Yaml::Str(s) => !s.is_empty(),
            _ => true,
        }
    }
}

fn get<'a>(map: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn set(map: &mut Map, key: &str, value: Yaml) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key.to_string(), value)),
    }
}

/// The value of an option, looking in the cell first and the defaults second.
/// A null counts as absent, like JavaScript's `??`.
fn option<'a>(cell: &'a [(String, Yaml)], defaults: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    get(cell, key)
        .filter(|v| **v != Yaml::Null)
        .or_else(|| get(defaults, key).filter(|v| **v != Yaml::Null))
}

fn is_number(t: &str) -> bool {
    let t = t.strip_prefix('-').unwrap_or(t);
    let (whole, fraction) = match t.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (t, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(whole)
        && match fraction {
            None => true,
            Some(f) => digits(f),
        }
}

fn unescape_double(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                _ => {
                    out.push('\\');
                    continue;
                }
            }
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn scalar(value: &str) -> Yaml {
    let t = value.trim();
    if t.is_empty() {
        return Yaml::Str(String::new());
    }
    match t.to_ascii_lowercase().as_str() {
        "true" => return Yaml::Bool(true),
        "false" => return Yaml::Bool(false),
        "null" | "~" => return Yaml::Null,
        _ => {}
    }
    if is_number(t) {
        if let Ok(n) = t.parse::<f64>() {
            return Yaml::Num(n);
        }
    }
    let bytes = t.as_bytes();
    if t.len() >= 2 {
        let (first, last) = (bytes[0], bytes[t.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            let inner = &t[1..t.len() - 1];
            return Yaml::Str(if first == b'"' {
                unescape_double(inner)
            } else {
                inner.replace("''", "'")
            });
        }
        if first == b'[' && last == b']' {
            return Yaml::List(t[1..t.len() - 1].split(',').map(scalar).collect());
        }
        if first == b'{' && last == b'}' {
            let mut out = Map::new();
            for item in t[1..t.len() - 1].split(',') {
                if let Some(at) = item.find(':') {
                    if at > 0 {
                        set(&mut out, item[..at].trim(), scalar(&item[at + 1..]));
                    }
                }
            }
            return Yaml::Map(out);
        }
    }
    Yaml::Str(t.to_string())
}

fn warning(key: &str, hints: Vec<String>, file: &str, line: usize, column: usize) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        message: key.to_string(),
        hints,
        file: file.to_string(),
        line,
        column,
        end_line: 0,
        end_column: 0,
    }
}

/// What the front matter said, and how many lines it takes.
pub struct FrontMatter {
    pub meta: Vec<(String, Yaml)>,
    /// Lines from the opening `---` to the closing one; 0 when there is none.
    pub lines: usize,
    pub diagnostics: Vec<Diagnostic>,
}

fn is_array_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "author" | "authors" | "keyword" | "keywords" | "bibliography" | "csl" | "filter" | "filters"
    )
}

/// `  - rest` gives the indent and what follows the dash.
fn list_item(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let rest = t.strip_prefix('-')?;
    Some((line.len() - t.len(), rest.trim_start()))
}

/// `  key: rest` gives the indent, the key and what follows the colon.
fn key_line(line: &str) -> Option<(usize, &str, &str)> {
    let t = line.trim_start();
    let first = t.chars().next()?;
    if first == ':' || first == '#' {
        return None;
    }
    let colon = t.find(':')?;
    Some((line.len() - t.len(), t[..colon].trim(), t[colon + 1..].trim_start()))
}

/// The small YAML reader for front matter: top-level scalars, one level of
/// nesting, and lists for the keys that take them. Unknown shapes are warned
/// about; the source itself is never rewritten.
pub fn parse_front_matter(lines: &[&str]) -> FrontMatter {
    let none = |diagnostics: Vec<Diagnostic>| FrontMatter { meta: Vec::new(), lines: 0, diagnostics };
    let first = lines.first().map(|l| l.trim_start_matches('\u{feff}').trim());
    if first != Some("---") {
        return none(Vec::new());
    }
    let close = (1..lines.len()).find(|&i| matches!(lines[i].trim(), "---" | "..."));
    let Some(close) = close else {
        // No safe boundary yet: leave the document alone and say so.
        return none(vec![warning("quarto_yaml_unfinished", vec![], "", 1, 1)]);
    };
    let mut top = Map::new();
    let mut diagnostics = Vec::new();
    let mut current_key: Option<String> = None;
    let mut array_key: Option<String> = None;
    // The map being filled inside a list: which list, and which item.
    let mut array_item: Option<(String, usize)> = None;
    let mut array_indent: isize = -1;
    for (i, line) in lines.iter().enumerate().take(close).skip(1) {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let (Some((indent, rest)), Some(ak)) = (list_item(line), array_key.clone()) {
            if !matches!(get(&top, &ak), Some(Yaml::List(_))) {
                set(&mut top, &ak, Yaml::List(Vec::new()));
            }
            let item = rest.trim();
            let entry = match item.find(':') {
                Some(at) if at > 0 => {
                    let mut m = Map::new();
                    set(&mut m, item[..at].trim(), scalar(&item[at + 1..]));
                    Some(Yaml::Map(m))
                }
                _ => None,
            };
            if let Some(Yaml::List(items)) = top.iter_mut().find(|(k, _)| *k == ak).map(|(_, v)| v) {
                array_item = entry.as_ref().map(|_| (ak.clone(), items.len()));
                items.push(entry.unwrap_or_else(|| scalar(item)));
            }
            array_indent = indent as isize;
            continue;
        }
        let Some((indent, key, rest)) = key_line(line) else {
            diagnostics.push(warning("quarto_yaml_parse_failed", vec![], "", i + 1, 1));
            continue;
        };
        if indent == 0 {
            current_key = Some(key.to_string());
            array_key = if rest.is_empty() && is_array_key(key) { Some(key.to_string()) } else { None };
            array_item = None;
            array_indent = -1;
            let value = if !rest.is_empty() {
                scalar(rest)
            } else if array_key.is_some() {
                Yaml::List(Vec::new())
            } else {
                Yaml::Map(Map::new())
            };
            set(&mut top, key, value);
        } else if let (Some((list, at)), true) = (array_item.clone(), indent as isize > array_indent) {
            if let Some(Yaml::List(items)) = top.iter_mut().find(|(k, _)| *k == list).map(|(_, v)| v) {
                if let Some(Yaml::Map(m)) = items.get_mut(at) {
                    set(m, key, scalar(rest));
                }
            }
        } else if let Some(Yaml::Map(m)) = current_key
            .as_ref()
            .and_then(|ck| top.iter_mut().find(|(k, _)| k == ck))
            .map(|(_, v)| v)
        {
            array_key = None;
            set(m, key, scalar(rest));
        } else {
            diagnostics.push(warning("quarto_yaml_nesting", vec![], "", i + 1, indent + 1));
        }
    }
    FrontMatter { meta: top, lines: close + 1, diagnostics }
}

// ------------------------------------------------------------ attributes

#[derive(Default)]
struct Attrs {
    id: String,
    attrs: Map,
}

/// End of a quoted run starting at `i`, or none when it is never closed.
fn quoted_end(b: &[u8], i: usize, quote: u8) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        if quote == b'"' && b[j] == b'\\' {
            if j + 1 >= b.len() {
                return None;
            }
            j += 2;
        } else if b[j] == quote {
            if quote == b'\'' && j + 1 < b.len() && b[j + 1] == b'\'' {
                j += 2;
            } else {
                return Some(j + 1);
            }
        } else {
            j += 1;
        }
    }
    None
}

/// Words separated by spaces, where a quoted run belongs to its word.
fn tokens(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            if b[i] == b'"' || b[i] == b'\'' {
                match quoted_end(b, i, b[i]) {
                    Some(end) => i = end,
                    None => break,
                }
            } else {
                i += 1;
            }
        }
        if i > start {
            out.push(&text[start..i]);
        } else {
            i += 1; // a quote that never closes
        }
    }
    out
}

/// `{#id .class key=value}`: the id and the key-value pairs. Classes are not
/// needed by the draft.
fn parse_attributes(input: &str) -> Attrs {
    let mut t = input.trim();
    t = t.strip_prefix('{').unwrap_or(t);
    t = t.strip_suffix('}').unwrap_or(t).trim();
    let mut out = Attrs::default();
    for token in tokens(t) {
        if let Some(id) = token.strip_prefix('#') {
            out.id = id.to_string();
        } else if !token.starts_with('.') {
            if let Some(at) = token.find('=') {
                if at > 0 {
                    set(&mut out.attrs, &token[..at], scalar(&token[at + 1..]));
                }
            }
        }
    }
    out
}

// -------------------------------------------------------------- the parse

/// A fence of three or more backticks or tildes: its character, its length
/// and what follows it.
fn fence_open(line: &str) -> Option<(u8, usize, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then(|| (ch, len, t[len..].trim()))
}

fn fence_closes(line: &str, ch: u8, len: usize) -> bool {
    match fence_open(line) {
        Some((c, l, rest)) => c == ch && l >= len && rest.is_empty(),
        None => false,
    }
}

/// `:::` fences: `Some("")` closes a div, `Some(info)` opens one.
fn div_fence(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let colons = t.bytes().take_while(|&b| b == b':').count();
    if colons < 3 {
        return None;
    }
    let rest = &t[colons..];
    if rest.trim().is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) || rest.starts_with('{') {
        Some(rest.trim())
    } else {
        None
    }
}

fn starts_with_language(inner: &str) -> bool {
    let mut chars = inner.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            continue;
        }
        return c.is_whitespace() || c == ',';
    }
    true
}

fn language_of(info: &str) -> String {
    let t = info.trim();
    let t = t.strip_prefix('{').unwrap_or(t);
    let t = t.strip_suffix('}').unwrap_or(t).trim();
    let word = t.split_whitespace().next().unwrap_or("");
    let word = word.strip_prefix('.').unwrap_or(word);
    let end = word.find([',', '{']).unwrap_or(word.len());
    word[..end].to_lowercase()
}

/// `#| key: value`.
fn option_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().strip_prefix("#|")?;
    let colon = rest.find(':')?;
    if colon == 0 {
        return None;
    }
    Some((rest[..colon].trim(), rest[colon + 1..].trim()))
}

/// `## Heading {#sec-id}` gives the id.
fn heading_id(line: &str) -> Option<String> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_end().strip_suffix('}')?;
    let at = rest.rfind("{#")?;
    let (before, id) = (&rest[..at], &rest[at + 2..]);
    if !before.ends_with(char::is_whitespace) || before.trim().is_empty() {
        return None;
    }
    let mut chars = id.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
        .then(|| id.to_string())
}

struct Cell {
    start: usize,
    end: usize,
    language: String,
    options: Map,
    label: String,
    /// The code, without the `#|` option lines.
    code: Vec<String>,
}

struct Div {
    start: usize,
    end: Option<usize>,
    depth: usize,
    id: String,
}

struct Parsed {
    fm: FrontMatter,
    cells: Vec<Cell>,
    divs: Vec<Div>,
    headings: Vec<(usize, String)>,
    diagnostics: Vec<Diagnostic>,
}

struct OpenFence {
    ch: u8,
    len: usize,
    info: String,
    start: usize,
}

/// Rewrites the commas that separate knitr-style chunk options into spaces.
fn knitr_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_alphabetic() {
                j += 1;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-') {
                    j += 1;
                }
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '=' {
                    out.push(' ');
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

fn build_cell(fence: &OpenFence, lines: &[&str], end: usize) -> Cell {
    let body = &lines[fence.start + 1..end];
    let mut options = Map::new();
    let mut skip = 0;
    while skip < body.len() {
        match option_line(body[skip]) {
            Some((key, value)) => set(&mut options, key, scalar(value)),
            None => break,
        }
        skip += 1;
    }
    let code: Vec<String> = body[skip..].iter().map(|s| s.to_string()).collect();
    let attribute_info = match fence.info.find('{') {
        Some(at) => &fence.info[at..],
        None => &fence.info[..],
    };
    let parsed = parse_attributes(attribute_info);
    let info_text = fence.info.strip_prefix('{').unwrap_or(&fence.info);
    let info_text = info_text.strip_suffix('}').unwrap_or(info_text).trim();
    // The language is the first word; what follows are chunk options.
    let mut rest = match info_text.find(|c: char| c == ',' || c.is_whitespace()) {
        Some(at) if at > 0 => &info_text[at..],
        _ => "",
    };
    if let Some(after) = rest.trim_start().strip_prefix(',') {
        rest = after.trim_start();
    }
    let info_parsed = parse_attributes(&knitr_commas(rest));
    let mut merged = info_parsed.attrs;
    for (key, value) in options {
        set(&mut merged, &key, value);
    }
    let label = match get(&merged, "label").filter(|v| v.truthy()) {
        Some(v) => v.text(),
        None if !info_parsed.id.is_empty() => info_parsed.id.clone(),
        None => parsed.id.clone(),
    };
    Cell { start: fence.start, end, language: language_of(&fence.info), options: merged, label, code }
}

fn parse(lines: &[&str]) -> Parsed {
    let fm = parse_front_matter(lines);
    let mut diagnostics = fm.diagnostics.clone();
    let mut cells: Vec<Cell> = Vec::new();
    let mut divs: Vec<Div> = Vec::new();
    let mut headings = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut fence: Option<OpenFence> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(open) = fence.take() {
            if fence_closes(line, open.ch, open.len) {
                cells.push(build_cell(&open, lines, i));
            } else {
                fence = Some(open);
            }
            i += 1;
            continue;
        }
        if let Some((ch, len, info)) = fence_open(line) {
            let inner = info.strip_prefix('{').unwrap_or(info);
            let inner = inner.strip_suffix('}').unwrap_or(inner).trim_start();
            if info.starts_with('{') && starts_with_language(inner) {
                let stripped = info.strip_prefix('{').unwrap_or(info);
                let stripped = stripped.strip_suffix('}').unwrap_or(stripped);
                fence = Some(OpenFence { ch, len, info: stripped.to_string(), start: i });
                i += 1;
                continue;
            }
            // Every other fence is opaque: a braced fence inside a
            // documentation example is not a live cell.
            let mut end = i + 1;
            while end < lines.len() && !fence_closes(lines[end], ch, len) {
                end += 1;
            }
            i = end.min(lines.len()) + 1;
            continue;
        }
        if let Some(id) = heading_id(line) {
            headings.push((i, id));
        }
        if let Some(info) = div_fence(line) {
            if info.is_empty() {
                if let Some(top) = stack.pop() {
                    divs[top].end = Some(i);
                }
            } else {
                divs.push(Div { start: i, end: None, depth: stack.len(), id: parse_attributes(info).id });
                stack.push(divs.len() - 1);
            }
        }
        i += 1;
    }
    if let Some(open) = &fence {
        diagnostics.push(warning("quarto_cell_unfinished", vec![], "", open.start + 1, 1));
    }
    if let Some(&top) = stack.last() {
        diagnostics.push(warning("quarto_div_unfinished", vec![], "", divs[top].start + 1, 1));
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for cell in cells.iter().filter(|c| !c.label.is_empty()) {
        *counts.entry(cell.label.as_str()).or_insert(0) += 1;
    }
    for cell in &cells {
        if !cell.label.is_empty() && counts.get(cell.label.as_str()).copied().unwrap_or(0) > 1 {
            diagnostics.push(warning(
                "quarto_cell_label_duplicate",
                vec![format!("label={}", cell.label)],
                "",
                cell.start + 1,
                1,
            ));
        }
    }
    Parsed { fm, cells, divs, headings, diagnostics }
}

// ------------------------------------------------------------- references

type Refs = HashMap<String, (&'static str, usize)>;

const KINDS: [&str; 5] = ["fig", "tbl", "sec", "eq", "lst"];

fn reference_kind(id: &str) -> Option<&'static str> {
    KINDS
        .into_iter()
        .find(|k| id.strip_prefix(k).is_some_and(|rest| rest.starts_with('-')))
}

fn noun(kind: &str) -> &'static str {
    match kind {
        "fig" => "Figure",
        "tbl" => "Table",
        "sec" => "Section",
        "eq" => "Equation",
        _ => "Listing",
    }
}

/// Every labelled target, numbered by kind in document order.
fn references(parsed: &Parsed) -> Refs {
    let mut targets: Vec<(usize, &str)> = Vec::new();
    targets.extend(parsed.headings.iter().map(|(l, id)| (*l, id.as_str())));
    targets.extend(parsed.divs.iter().map(|d| (d.start, d.id.as_str())));
    targets.extend(parsed.cells.iter().map(|c| (c.start, c.label.as_str())));
    targets.retain(|&(_, id)| reference_kind(id).is_some());
    targets.sort_by_key(|&(line, _)| line);
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut out = Refs::new();
    for (_, id) in targets {
        if out.contains_key(id) {
            continue;
        }
        let kind = reference_kind(id).unwrap_or("fig");
        let number = counts.entry(kind).or_insert(0);
        *number += 1;
        out.insert(id.to_string(), (kind, *number));
    }
    out
}

fn label_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b == b'-'
}

/// `@fig-x` and friends in one stretch of prose.
fn crossrefs(text: &str, refs: &Refs) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('@') {
        let start = i + at;
        i = start + 1;
        let Some(kind) = KINDS.into_iter().find(|k| {
            text[start + 1..].strip_prefix(k).is_some_and(|rest| rest.starts_with('-'))
        }) else {
            continue;
        };
        let mut end = start + 1 + kind.len() + 1;
        let label_start = end;
        while end < b.len() && label_char(b[end]) {
            end += 1;
        }
        if end == label_start {
            continue;
        }
        while end + 1 < b.len() && b[end] == b'.' && label_char(b[end + 1]) {
            end += 1;
            while end < b.len() && label_char(b[end]) {
                end += 1;
            }
        }
        let id = &text[start + 1..end];
        if let Some((kind, number)) = refs.get(id) {
            out.push_str(&text[copied..start]);
            out.push_str(&format!(
                "<a class=\"quarto-crossref\" href=\"#{}\">{} {}</a>",
                escape(id),
                noun(kind),
                number
            ));
            copied = end;
        }
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// `[text]{#id}` becomes a span with that id.
fn spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('[') {
        let start = i + at;
        i = start + 1;
        let Some(close) = text[start + 1..].find(']') else { break };
        let close = start + 1 + close;
        if close == start + 1 || !text[close + 1..].starts_with("{#") {
            continue;
        }
        let id_start = close + 3;
        let Some(brace) = text[id_start..].find('}') else { continue };
        let id = &text[id_start..id_start + brace];
        let mut chars = id.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'));
        if !valid {
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str(&format!("<span id=\"{}\">{}</span>", escape(id), &text[start + 1..close]));
        copied = id_start + brace + 1;
        i = copied;
    }
    out.push_str(&text[copied..]);
    out
}

/// The end of a link or image starting at `i`, if one does.
fn link_end(line: &str, i: usize) -> Option<usize> {
    let b = line.as_bytes();
    let mut j = i;
    if b[j] == b'!' {
        j += 1;
    }
    if j >= b.len() || b[j] != b'[' {
        return None;
    }
    let close = j + 1 + line[j + 1..].find(']')?;
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    Some(close + 2 + line[close + 2..].find(')')? + 1)
}

/// Makes the common cross references readable. Inline code and links are
/// opaque; an unresolved reference stays source text.
fn prose(line: &str, refs: &Refs) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut text_start = 0;
    let mut i = 0;
    while i < b.len() {
        let end = match b[i] {
            b'`' => line[i + 1..].find('`').map(|k| i + 1 + k + 1),
            b'!' | b'[' => link_end(line, i),
            _ => None,
        };
        match end {
            Some(e) => {
                out.push_str(&spans(&crossrefs(&line[text_start..i], refs)));
                out.push_str(&line[i..e]);
                i = e;
                text_start = e;
            }
            None => i += 1,
        }
    }
    out.push_str(&spans(&crossrefs(&line[text_start..], refs)));
    out
}

// --------------------------------------------------------------- the draft

/// What the host knows that the source does not: the saved outputs of cells
/// (matched to cells by the host), a note about where they came from, and the
/// interface strings the draft prints.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub outputs: Vec<Output>,
    pub note: String,
    pub strings: Strings,
}

#[derive(Clone, Debug)]
pub struct Output {
    /// The 1-based line of the cell's opening fence in the main file.
    pub line: usize,
    pub html: String,
    pub stale: bool,
}

#[derive(Clone, Debug)]
pub struct Strings {
    pub output_stale: String,
    pub include_unavailable: String,
    pub include_cycle: String,
    pub abstract_label: String,
}

impl Default for Strings {
    fn default() -> Self {
        Strings {
            output_stale: "Output from an earlier version of this cell".into(),
            include_unavailable: "Include {name} is not available".into(),
            include_cycle: "Include cycle at {normalized}".into(),
            abstract_label: "Abstract".into(),
        }
    }
}

/// Where the host puts the results it matched.
pub const EXTRAS_PATH: &str = ".librepaper/quarto.json";

impl Extras {
    /// Reads `.librepaper/quarto.json` through the file map. A missing or
    /// unreadable file means no outputs, no note and English strings.
    pub fn load(files: &dyn Fn(&str) -> Option<String>) -> Extras {
        files(EXTRAS_PATH).map_or_else(Extras::default, |text| Extras::from_json(&text))
    }

    pub fn from_json(text: &str) -> Extras {
        let mut extras = Extras::default();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return extras;
        };
        if let Some(list) = value.get("outputs").and_then(|v| v.as_array()) {
            for item in list {
                let line = item.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let html = item.get("html").and_then(|v| v.as_str());
                if let (true, Some(html)) = (line > 0, html) {
                    extras.outputs.push(Output {
                        line,
                        html: html.to_string(),
                        stale: item.get("stale").and_then(|v| v.as_bool()).unwrap_or(false),
                    });
                }
            }
        }
        if let Some(note) = value.get("note").and_then(|v| v.as_str()) {
            extras.note = note.to_string();
        }
        if let Some(strings) = value.get("strings") {
            let read = |key: &str, slot: &mut String| {
                if let Some(s) = strings.get(key).and_then(|v| v.as_str()) {
                    *slot = s.to_string();
                }
            };
            read("output_stale", &mut extras.strings.output_stale);
            read("include_unavailable", &mut extras.strings.include_unavailable);
            read("include_cycle", &mut extras.strings.include_cycle);
            read("abstract", &mut extras.strings.abstract_label);
        }
        extras
    }
}

/// A Markdown draft and where each of its lines came from.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub markdown: String,
    /// Per draft line, the 1-based source line it came from; none for lines
    /// the draft generated.
    pub line_map: Vec<Option<usize>>,
    /// Per draft line, the 1-based inclusive range of source lines it belongs
    /// to, generated lines included: a title block belongs to the front
    /// matter, a cell's output to the cell.
    pub origin: Vec<(usize, usize)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Draft {
    /// Plain Markdown is its own draft.
    pub fn identity(source: &str) -> Draft {
        let count = source.split('\n').count();
        Draft {
            markdown: source.to_string(),
            line_map: (1..=count).map(Some).collect(),
            origin: (1..=count).map(|n| (n, n)).collect(),
            diagnostics: Vec::new(),
        }
    }

    /// `{"markdown": "...", "line_map": [1, null, 2]}`.
    pub fn to_json(&self) -> String {
        serde_json::json!({ "markdown": self.markdown, "line_map": self.line_map }).to_string()
    }
}

#[derive(Default)]
struct Out {
    lines: Vec<String>,
    map: Vec<Option<usize>>,
    origin: Vec<(usize, usize)>,
}

impl Out {
    /// Adds text, one draft line per line of it. `source` is the 0-based
    /// source line of its first line, if it has one; `origin` is the range
    /// a generated line belongs to.
    fn push(&mut self, text: &str, source: Option<usize>, origin: (usize, usize)) {
        for (n, part) in text.split('\n').enumerate() {
            self.lines.push(part.to_string());
            self.map.push(source.map(|s| s + n));
            self.origin.push(source.map_or(origin, |s| (s + n, s + n)));
        }
    }

    /// Generated HTML with blank lines around it, so it is a block of its own.
    fn block(&mut self, html: &str, origin: (usize, usize)) {
        self.push("", None, origin);
        self.push(html, None, origin);
        self.push("", None, origin);
    }
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        if ch == c {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// A fence for showing text verbatim that nothing inside it can close early.
fn verbatim_fence(text: &str) -> String {
    let backticks = 3.max(longest_run(text, '`') + 1);
    let tildes = 3.max(longest_run(text, '~') + 1);
    if backticks <= tildes {
        "`".repeat(backticks)
    } else {
        "~".repeat(tildes)
    }
}

fn title_block(meta: &Map, abstract_label: &str) -> String {
    let field = |key: &str| match get(meta, key) {
        None | Some(Yaml::Null) => String::new(),
        Some(v) => v.text().trim().to_string(),
    };
    let (author, authors) = (get(meta, "author"), get(meta, "authors"));
    let list: Vec<Yaml> = if let Some(Yaml::List(l)) = author {
        l.clone()
    } else if let Some(Yaml::List(l)) = authors {
        l.clone()
    } else if let Some(a) = author.filter(|a| a.truthy()) {
        vec![a.clone()]
    } else if let Some(a) = authors.filter(|a| a.truthy()) {
        vec![a.clone()]
    } else {
        Vec::new()
    };
    let names: Vec<String> = list
        .iter()
        .map(|a| match a {
            Yaml::Str(s) => s.clone(),
            Yaml::Map(m) => ["name", "literal"]
                .into_iter()
                .filter_map(|k| get(m, k).filter(|v| v.truthy()))
                .map(Yaml::text)
                .next()
                .unwrap_or_default(),
            _ => String::new(),
        })
        .filter(|n| !n.is_empty())
        .collect();
    let (title, date, abstract_text) = (field("title"), field("date"), field("abstract"));
    if title.is_empty() && names.is_empty() && date.is_empty() && abstract_text.is_empty() {
        return String::new();
    }
    let mut out = String::from("<header class=\"quarto-title-block\" data-librepaper-generated=\"quarto\">");
    if !title.is_empty() {
        out.push_str(&format!("<h1>{}</h1>", escape(&title)));
    }
    if !names.is_empty() {
        out.push_str(&format!("<p class=\"quarto-author\">{}</p>", escape(&names.join(", "))));
    }
    if !date.is_empty() {
        out.push_str(&format!("<p class=\"quarto-date\">{}</p>", escape(&date)));
    }
    if !abstract_text.is_empty() {
        out.push_str(&format!(
            "<div class=\"quarto-abstract\"><strong>{}</strong><p>{}</p></div>",
            escape(abstract_label),
            escape(&abstract_text)
        ));
    }
    out.push_str("</header>");
    out
}

/// `{{< include name >}}` at or after `from`: where it starts and ends, and
/// the name.
fn find_include(s: &str, from: usize) -> Option<(usize, usize, &str)> {
    let mut pos = from;
    while let Some(k) = s[pos..].find("{{<") {
        let start = pos + k;
        pos = start + 3;
        let after = &s[pos..];
        let j = pos + (after.len() - after.trim_start().len());
        let Some(rest) = s[j..].strip_prefix("include") else { continue };
        let name_at = j + 7 + (rest.len() - rest.trim_start().len());
        if name_at == j + 7 {
            continue;
        }
        let name_len = s[name_at..].find([' ', '>']).unwrap_or(s.len() - name_at);
        if name_len == 0 {
            continue;
        }
        let name_end = name_at + name_len;
        if let Some(g) = s[name_end..].find(">}}") {
            return Some((start, name_end + g + 3, &s[name_at..name_end]));
        }
    }
    None
}

/// Whether a line holds a shortcode other than `include`.
fn has_unsupported_shortcode(line: &str) -> bool {
    let mut pos = 0;
    while let Some(k) = line[pos..].find("{{<") {
        pos += k + 3;
        let rest = line[pos..].trim_start();
        let name_len = rest
            .char_indices()
            .take_while(|&(n, c)| {
                if n == 0 {
                    c.is_ascii_alphabetic()
                } else {
                    c.is_ascii_alphanumeric() || c == '_' || c == '-'
                }
            })
            .count();
        if name_len == 0 {
            continue;
        }
        let after = &rest[name_len..];
        if let Some(g) = after.find('>') {
            if after[g + 1..].starts_with("}}") && !rest[..name_len].eq_ignore_ascii_case("include") {
                return true;
            }
        }
    }
    false
}

struct Ctx<'a> {
    path: &'a str,
    files: &'a dyn Fn(&str) -> Option<String>,
    strings: &'a Strings,
    defaults: &'a Map,
    diagnostics: Vec<Diagnostic>,
}

impl Ctx<'_> {
    /// Finds an include's file: relative to the including file, never
    /// absolute, never climbing above the root.
    fn resolve(&self, name: &str, source_name: &str) -> Option<(String, String)> {
        let requested = name.replace('\\', "/");
        let drive = requested.as_bytes().get(1) == Some(&b':')
            && requested.as_bytes().first().is_some_and(|b| b.is_ascii_alphabetic());
        if requested.is_empty() || requested.starts_with('/') || drive || requested.split('/').any(str::is_empty) {
            return None;
        }
        let mut parts: Vec<&str> = source_name.split('/').collect();
        parts.pop();
        let mut normalized: Vec<&str> = Vec::new();
        for part in parts.into_iter().chain(requested.split('/')) {
            match part {
                "." => {}
                ".." => {
                    normalized.pop()?;
                }
                _ => normalized.push(part),
            }
        }
        let normalized = normalized.join("/");
        (self.files)(&normalized)
            .map(|text| (normalized.clone(), text))
            .or_else(|| (self.files)(&requested).map(|text| (requested.clone(), text)))
    }

    fn report(&mut self, key: &str, hint: String, source_name: &str, line: usize) {
        let file = if source_name == self.path { "" } else { source_name };
        self.diagnostics.push(warning(key, vec![hint], file, line, usize::from(line > 0)));
    }

    fn include(&mut self, name: &str, depth: usize, seen: &[String], source_name: &str, line: usize) -> String {
        let Some((normalized, text)) = self.resolve(name, source_name) else {
            self.report("quarto_include_unauthorized", format!("name={name}"), source_name, line);
            let message = self.strings.include_unavailable.replace("{name}", &escape(name));
            return format!("<span class=\"quarto-diagnostic\">{message}</span>");
        };
        if depth >= MAX_INCLUDE_DEPTH || seen.contains(&normalized) {
            self.report("quarto_include_cycle", format!("normalized={normalized}"), source_name, line);
            let message = self.strings.include_cycle.replace("{normalized}", &escape(&normalized));
            return format!("<span class=\"quarto-diagnostic\">{message}</span>");
        }
        let mut next = seen.to_vec();
        next.push(normalized.clone());
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        let parsed = {
            let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
            parse(&borrowed)
        };
        let mut defaults = self.defaults.clone();
        if let Some(Yaml::Map(m)) = get(&parsed.fm.meta, "execute") {
            for (k, v) in m {
                set(&mut defaults, k, v.clone());
            }
        }
        // Cells an include hides in Quarto's output are hidden here too.
        for cell in &parsed.cells {
            let hidden = |key: &str| option(&cell.options, &defaults, key) == Some(&Yaml::Bool(false));
            if hidden("include") || hidden("echo") {
                for l in &mut lines[cell.start..=cell.end] {
                    l.clear();
                }
            }
        }
        let mut fence: Option<(u8, usize)> = None;
        let mut out = Vec::with_capacity(lines.len());
        for line in &lines {
            if let Some((ch, len)) = fence {
                if fence_closes(line, ch, len) {
                    fence = None;
                }
                out.push(line.clone());
                continue;
            }
            if let Some((ch, len, _)) = fence_open(line) {
                fence = Some((ch, len));
            }
            out.push(self.replace_includes(line, depth + 1, &next, &normalized, 0));
        }
        out.join("\n")
    }

    fn replace_includes(&mut self, line: &str, depth: usize, seen: &[String], source_name: &str, source_line: usize) -> String {
        let mut out = String::with_capacity(line.len());
        let mut at = 0;
        while let Some((start, end, name)) = find_include(line, at) {
            out.push_str(&line[at..start]);
            let expanded = self.include(name, depth, seen, source_name, source_line);
            out.push_str(&expanded);
            at = end;
        }
        out.push_str(&line[at..]);
        out
    }
}

/// Composes the draft of a Quarto source.
///
/// `path` is the main file's project path, `files` reads the files the host
/// handed over (for includes), and `extras` carries saved outputs. With
/// `for_pandoc` the original front matter is kept, no title block is made and
/// no output or note is placed: that is the text Pandoc reads for a PDF.
pub fn draft(
    source: &str,
    path: &str,
    files: &dyn Fn(&str) -> Option<String>,
    extras: &Extras,
    for_pandoc: bool,
) -> Draft {
    let lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let parsed = parse(&lines);
    let defaults: Map = match get(&parsed.fm.meta, "execute") {
        Some(Yaml::Map(m)) => m.clone(),
        _ => Vec::new(),
    };
    let refs = references(&parsed);
    let mut ctx = Ctx { path, files, strings: &extras.strings, defaults: &defaults, diagnostics: Vec::new() };
    let mut out = Out::default();

    let cell_at: HashMap<usize, &Cell> = parsed.cells.iter().map(|c| (c.start, c)).collect();
    let div_at: HashMap<usize, Option<usize>> = parsed
        .divs
        .iter()
        .filter(|d| d.depth == 0)
        .map(|d| (d.start, d.end))
        .collect();
    // Outputs belong to cells Quarto executes, matched by the opening line.
    let mut outputs: HashMap<usize, &Output> = HashMap::new();
    if !for_pandoc {
        for cell in &parsed.cells {
            if option(&cell.options, &defaults, "include") == Some(&Yaml::Bool(false)) {
                continue;
            }
            if let Some(o) = extras.outputs.iter().find(|o| o.line == cell.start + 1) {
                outputs.insert(cell.start, o);
            }
        }
    }
    let note = extras.note.trim();
    let note_markup = if note.is_empty() || for_pandoc {
        String::new()
    } else {
        format!("<p class=\"lp-results-note\">{}</p>", escape(note))
    };

    let front = parsed.fm.lines;
    if front == 0 && !note_markup.is_empty() {
        out.block(&note_markup, (0, 0));
    }
    let mut opaque: Option<(u8, usize)> = None;
    let mut i = 0;
    while i < lines.len() {
        if i < front {
            if for_pandoc {
                out.push(lines[i], Some(i), (i, i));
                if i == front - 1 {
                    out.push("", None, (0, i));
                }
            } else if i == front - 1 {
                let header = title_block(&parsed.fm.meta, &extras.strings.abstract_label);
                if !header.is_empty() {
                    out.push(&header, None, (0, i));
                }
                if !note_markup.is_empty() {
                    out.block(&note_markup, (0, i));
                }
            }
            i += 1;
            continue;
        }
        let Some(cell) = cell_at.get(&i).copied() else {
            if let Some(end) = div_at.get(&i) {
                // A fenced div is never interpreted: shown whole, as source.
                let end = end.unwrap_or(lines.len() - 1);
                let text = lines[i..=end].join("\n");
                let fence = verbatim_fence(&text);
                out.push(&fence, None, (i, end));
                out.push(&text, Some(i), (i, end));
                out.push(&fence, None, (i, end));
                i = end + 1;
                continue;
            }
            let line = lines[i];
            let mapped = if line.contains("{{<") { None } else { Some(i) };
            if let Some((ch, len)) = opaque {
                out.push(line, Some(i), (i, i));
                if fence_closes(line, ch, len) {
                    opaque = None;
                }
            } else if let Some((ch, len, _)) = fence_open(line) {
                opaque = Some((ch, len));
                out.push(line, mapped, (i, i));
            } else if div_fence(line).is_some() {
                out.push(line, mapped, (i, i));
            } else if has_unsupported_shortcode(line) {
                // Kept whole, so Markdown cannot turn the fragments around
                // it into rendered HTML.
                let fence = verbatim_fence(line);
                out.push(&fence, None, (i, i));
                out.push(line, Some(i), (i, i));
                out.push(&fence, None, (i, i));
            } else {
                let expanded = ctx.replace_includes(line, 0, &[], path, i + 1);
                // Included text has no one-to-one source line.
                out.push(&prose(&expanded, &refs), mapped, (i, i));
            }
            i += 1;
            continue;
        };
        let span = (cell.start, cell.end);
        match outputs.get(&cell.start) {
            Some(output) => {
                // Shown as Quarto does: the code without its fence or `#|`
                // lines, unless echo is off, then the output.
                let echo = option(&cell.options, &defaults, "echo");
                let shown = !matches!(echo, Some(Yaml::Bool(false)))
                    && !matches!(echo, Some(Yaml::Str(s)) if s == "false");
                let code = cell.code.join("\n");
                if shown && !code.trim().is_empty() {
                    let body = cell.end - cell.start - 1;
                    let first = cell.start + 1 + body.saturating_sub(cell.code.len());
                    let fence = verbatim_fence(&code);
                    out.push(&format!("{fence}{}", cell.language), None, span);
                    for (n, line) in cell.code.iter().enumerate() {
                        out.push(line, Some(first + n), span);
                    }
                    out.push(&fence, None, span);
                }
                let html = output.html.replace("\r\n", " ").replace('\n', " ");
                let block = if output.stale {
                    format!(
                        "<div class=\"lp-cell-output lp-cell-output-stale\" title=\"{}\">{html}</div>",
                        escape(&extras.strings.output_stale)
                    )
                } else {
                    format!("<div class=\"lp-cell-output\">{html}</div>")
                };
                out.block(&block, span);
            }
            None => {
                let text = lines[cell.start..=cell.end].join("\n");
                let fence = verbatim_fence(&text);
                out.push(&fence, None, span);
                out.push(&text, Some(cell.start), span);
                out.push(&fence, None, span);
            }
        }
        i = cell.end + 1;
    }

    let mut diagnostics = parsed.diagnostics.clone();
    diagnostics.append(&mut ctx.diagnostics);
    Draft {
        markdown: out.lines.join("\n"),
        line_map: out.map.iter().map(|m| m.map(|n| n + 1)).collect(),
        origin: out.origin.iter().map(|&(a, b)| (a + 1, b + 1)).collect(),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_files(_: &str) -> Option<String> {
        None
    }

    fn compose(source: &str) -> Draft {
        draft(source, "main.qmd", &no_files, &Extras::default(), false)
    }

    fn with_files(source: &str, files: &[(&str, &str)]) -> Draft {
        let table: Vec<(String, String)> = files.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let read = move |p: &str| table.iter().find(|(k, _)| k == p).map(|(_, v)| v.clone());
        draft(source, "main.qmd", &read, &Extras::default(), false)
    }

    fn with_outputs(source: &str, json: &str) -> Draft {
        draft(source, "paper.qmd", &no_files, &Extras::from_json(json), false)
    }

    #[test]
    fn detects_quarto_by_extension() {
        assert!(is_quarto("paper.qmd"));
        assert!(is_quarto("A/B.QMD"));
        assert!(!is_quarto("paper.md"));
    }

    #[test]
    fn a_cell_without_output_is_shown_verbatim_in_a_fence_that_cannot_close_early() {
        let d = compose("```{r,echo=FALSE}\nhidden_one()\n```\n").markdown;
        assert!(d.contains("hidden_one"));
        assert!(d.contains("```{r,echo=FALSE}"));
        assert!(d.starts_with("~~~\n"), "{d}");
    }

    #[test]
    fn a_div_is_one_literal_block() {
        assert_eq!(
            compose(":::{.callout-note}\nCompact note.\n:::\n").markdown,
            "```\n:::{.callout-note}\nCompact note.\n:::\n```\n"
        );
        let d = compose("::: {.callout-note}\n<div>\n  first\n</div>\n:::\n").markdown;
        assert!(d.starts_with("```\n::: {.callout-note}\n<div>\n  first\n</div>\n:::\n```"), "{d}");
        assert_eq!(
            compose("::: {.callout-note}\nStill typing\n  indented").markdown,
            "```\n::: {.callout-note}\nStill typing\n  indented\n```"
        );
        let nested = compose("::: {.a}\nOuter\n::: {.b}\nInner\n:::\n:::\n").markdown;
        assert_eq!(nested.matches("```").count(), 2, "only the outer div is wrapped: {nested}");
    }

    #[test]
    fn an_unsupported_shortcode_is_kept_with_its_line() {
        assert_eq!(
            compose("Before {{< video clip.mp4 >}} after\n").markdown,
            "```\nBefore {{< video clip.mp4 >}} after\n```\n"
        );
    }

    #[test]
    fn front_matter_becomes_a_title_block() {
        let source = "---\ntitle: Draft title\nauthors:\n  - name: Ada Lovelace\ndate: 1843\nabstract: A short abstract\n---\n\nBody\n";
        let d = compose(source);
        assert!(d.markdown.starts_with("<header class=\"quarto-title-block\""), "{}", d.markdown);
        assert!(d.markdown.contains("<h1>Draft title</h1>"));
        assert!(d.markdown.contains("<p class=\"quarto-author\">Ada Lovelace</p>"));
        assert!(d.markdown.contains("<p class=\"quarto-date\">1843</p>"));
        assert!(d.markdown.contains("<strong>Abstract</strong><p>A short abstract</p>"));
        assert!(!d.markdown.contains("\n---\n"));
        // The generated line has no source line; the prose keeps its own.
        assert_eq!(d.line_map[0], None);
        assert_eq!(d.line_map.last(), Some(&Some(10)));
        assert_eq!(d.origin[0], (1, 7), "the title block belongs to the front matter");
    }

    #[test]
    fn cross_references_are_numbered_in_document_order() {
        let source = "::: {.callout-note #intro}\nSee @fig-trend.\n:::\n\n\
                      ```{r #fig-trend}\n#| fig-cap: Trend\nplot(1)\n```\n\n\
                      See @fig-trend and @tbl-one and @fig-nope, `@fig-trend` and [@fig-trend](x).\n\n\
                      ::: {#tbl-one}\nTable caption\n:::\n";
        let d = compose(source).markdown;
        assert!(d.contains("See @fig-trend.\n"), "inside a literal div it stays source: {d}");
        assert!(d.contains("<a class=\"quarto-crossref\" href=\"#fig-trend\">Figure 1</a> and "), "{d}");
        assert!(d.contains("href=\"#tbl-one\">Table 1</a>"), "{d}");
        assert!(d.contains("@fig-nope"), "unresolved stays text: {d}");
        assert!(d.contains("`@fig-trend`"), "inline code is untouched: {d}");
        assert!(d.contains("[@fig-trend](x)"), "links are untouched: {d}");
    }

    #[test]
    fn headings_and_spans_resolve() {
        let d = compose("# Intro {#sec-intro}\n\nSee @sec-intro and [marked]{#mk}.\n").markdown;
        assert!(d.contains("href=\"#sec-intro\">Section 1</a>"), "{d}");
        assert!(d.contains("<span id=\"mk\">marked</span>"), "{d}");
    }

    #[test]
    fn opaque_fences_are_not_cells_and_not_rewritten() {
        let d = compose("````\n```{r}\nplot(@fig-x)\n```\n````\n").markdown;
        assert_eq!(d, "````\n```{r}\nplot(@fig-x)\n```\n````\n");
        let d = with_files("```text\n{{< include hidden.qmd >}}\n```\n", &[("hidden.qmd", "DO NOT INSERT")]);
        assert!(d.markdown.contains("{{< include hidden.qmd >}}"));
        assert!(!d.markdown.contains("DO NOT INSERT"));
    }

    #[test]
    fn includes_expand_and_nest_and_stay_opaque_in_code() {
        let d = with_files(
            "Before {{< include outer.qmd >}} after.",
            &[
                ("outer.qmd", "Outer {{< include inner.qmd >}}\n\n```{r}\n{{< include secret.qmd >}}\n```"),
                ("inner.qmd", "Inner text"),
                ("secret.qmd", "SECRET"),
            ],
        );
        assert!(d.markdown.contains("Outer Inner text"), "{}", d.markdown);
        assert!(d.markdown.contains("{{< include secret.qmd >}}"));
        assert!(!d.markdown.contains("SECRET"));
        // Included text has no one-to-one source line.
        assert!(d.line_map.iter().all(Option::is_none));
    }

    #[test]
    fn include_cycles_and_unavailable_files_are_reported() {
        let d = with_files("{{< include a.qmd >}}", &[("a.qmd", "A {{< include b.qmd >}}"), ("b.qmd", "B {{< include a.qmd >}}")]);
        assert!(d.markdown.contains("Include cycle at a.qmd"), "{}", d.markdown);
        let cycle = d.diagnostics.iter().find(|x| x.message == "quarto_include_cycle").unwrap();
        assert_eq!(cycle.hints, vec!["normalized=a.qmd".to_string()]);
        assert_eq!(cycle.file, "b.qmd");

        let d = with_files("{{< include ../secret.qmd >}}", &[("../secret.qmd", "must never appear"), ("safe.qmd", "safe")]);
        assert!(!d.markdown.contains("must never appear"));
        assert!(d.markdown.contains("Include ../secret.qmd is not available"));
        let unauthorized = &d.diagnostics[0];
        assert_eq!(unauthorized.message, "quarto_include_unauthorized");
        assert_eq!(unauthorized.hints, vec!["name=../secret.qmd".to_string()]);
        assert_eq!((unauthorized.file.as_str(), unauthorized.line), ("", 1));
    }

    #[test]
    fn an_included_hidden_cell_is_dropped() {
        let d = with_files(
            "{{< include hidden.qmd >}}",
            &[("hidden.qmd", "```{r}\n#| include: false\nsecret_hidden_code()\n```\nVisible prose")],
        );
        assert!(!d.markdown.contains("secret_hidden_code"));
        assert!(d.markdown.contains("Visible prose"));
    }

    #[test]
    fn includes_resolve_relative_to_the_including_file() {
        let read = |p: &str| (p == "chap/part.qmd").then(|| "Part".to_string());
        let d = draft("{{< include chap/part.qmd >}}", "main.qmd", &read, &Extras::default(), false);
        assert_eq!(d.markdown, "Part");
    }

    #[test]
    fn yaml_nesting_and_warnings() {
        let lines = [
            "---",
            "authors:",
            "  - name: Ada Lovelace",
            "    affiliation: Analytical Engine",
            "  - name: Grace Hopper",
            "execute:",
            "  echo: false",
            "---",
        ];
        let fm = parse_front_matter(&lines);
        assert!(fm.diagnostics.is_empty(), "{:?}", fm.diagnostics);
        assert_eq!(fm.lines, 8);
        let Some(Yaml::List(authors)) = get(&fm.meta, "authors") else { panic!() };
        assert_eq!(authors.len(), 2);
        let Yaml::Map(first) = &authors[0] else { panic!() };
        assert_eq!(get(first, "affiliation"), Some(&Yaml::Str("Analytical Engine".into())));
        assert_eq!(get(&fm.meta, "execute"), Some(&Yaml::Map(vec![("echo".into(), Yaml::Bool(false))])));

        let unfinished = compose("---\ntitle: Still editing\n\n# Intro\n\nText");
        assert!(unfinished.markdown.contains("Still editing"));
        assert_eq!(unfinished.diagnostics[0].message, "quarto_yaml_unfinished");
        assert_eq!((unfinished.diagnostics[0].line, unfinished.diagnostics[0].column), (1, 1));

        let fm = parse_front_matter(&["---", "title: x", "oops", "---"]);
        assert_eq!(fm.diagnostics[0].message, "quarto_yaml_parse_failed");
        assert_eq!(fm.diagnostics[0].line, 3);
        let fm = parse_front_matter(&["---", "title: x", "  nested: y", "---"]);
        assert_eq!(fm.diagnostics[0].message, "quarto_yaml_nesting");
    }

    #[test]
    fn yaml_scalars() {
        assert_eq!(scalar("TRUE"), Yaml::Bool(true));
        assert_eq!(scalar("~"), Yaml::Null);
        assert_eq!(scalar("-1.5"), Yaml::Num(-1.5));
        assert_eq!(scalar("\"a\\nb\""), Yaml::Str("a\nb".into()));
        assert_eq!(scalar("'it''s'"), Yaml::Str("it's".into()));
        assert_eq!(scalar("[a, 2]"), Yaml::List(vec![Yaml::Str("a".into()), Yaml::Num(2.0)]));
        assert_eq!(scalar("éa"), Yaml::Str("éa".into()));
        assert_eq!(Yaml::Num(1843.0).text(), "1843");
    }

    #[test]
    fn cell_and_div_diagnostics() {
        let d = compose("```{r #a}\n1\n```\n\n```{r #a}\n2\n```\n\n```{r}\nopen\n");
        let keys: Vec<&str> = d.diagnostics.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(keys, ["quarto_cell_unfinished", "quarto_cell_label_duplicate", "quarto_cell_label_duplicate"]);
        assert_eq!(d.diagnostics[1].hints, vec!["label=a".to_string()]);
        let d = compose("::: {.x}\ntext\n");
        assert_eq!(d.diagnostics[0].message, "quarto_div_unfinished");
    }

    // Lines: the first cell opens on 5, the second on 12, the hidden one on 17.
    const RESULTS: &str = "---\ntitle: Shared\n---\n\n```{r}\n#| label: first\na <- 1\n```\n\nBetween.\n\n```{r}\n#| label: second\nb <- 2\n```\n\n```{r}\n#| label: hidden\n#| include: false\nc <- 3\n```\n";

    #[test]
    fn saved_output_follows_its_cell() {
        let d = with_outputs(
            RESULTS,
            r#"{"outputs":[{"line":5,"html":"<p>ONE</p>"},{"line":12,"html":"<p>TWO\nlines</p>","stale":true}],
                "strings":{"output_stale":"Old <one>"}}"#,
        );
        let lines: Vec<&str> = d.markdown.split('\n').collect();
        assert!(lines.contains(&"```r"));
        assert!(!lines.contains(&"#| label: first"), "no option lines when output is shown");
        let position = |text: &str| lines.iter().position(|l| *l == text).unwrap();
        let one = position("<div class=\"lp-cell-output\"><p>ONE</p></div>");
        assert_eq!((lines[one - 1], lines[one + 1]), ("", ""), "generated HTML is a block of its own");
        assert!(position("a <- 1") < one);
        assert!(one < position("Between."));
        assert_eq!(d.line_map[one], None);
        assert_eq!(d.line_map[position("a <- 1")], Some(7));
        assert_eq!(d.line_map[position("```r")], None);
        assert!(d.markdown.contains(
            "<div class=\"lp-cell-output lp-cell-output-stale\" title=\"Old &lt;one&gt;\"><p>TWO lines</p></div>"
        ));
        assert_eq!(d.origin[one], (5, 8), "the output belongs to its cell");
    }

    #[test]
    fn a_hidden_cell_gets_no_output_and_stays_verbatim() {
        let d = with_outputs(RESULTS, r#"{"outputs":[{"line":17,"html":"<p>HIDDEN</p>"}]}"#);
        assert!(!d.markdown.contains("HIDDEN"));
        assert!(d.markdown.contains("#| label: hidden"));
        let bare = compose(RESULTS);
        assert!(bare.markdown.contains("#| label: first"));
        assert!(bare.markdown.contains("{r}"));
    }

    #[test]
    fn echo_decides_whether_the_code_is_shown() {
        let run = |front: &str, opt: &str| {
            let source = format!("---\ntitle: E\n{front}---\n\n```{{r}}\n#| label: only\n{opt}shown_code()\n```\n");
            let line = source.split('\n').position(|l| l == "```{r}").unwrap() + 1;
            let json = format!(r#"{{"outputs":[{{"line":{line},"html":"<p>OUT</p>"}}]}}"#);
            draft(&source, "e.qmd", &no_files, &Extras::from_json(&json), false).markdown
        };
        assert!(run("", "").contains("shown_code()"));
        let hidden = run("", "#| echo: false\n");
        assert!(!hidden.contains("shown_code"));
        assert!(hidden.contains("OUT"));
        assert!(!run("execute:\n  echo: false\n", "").contains("shown_code"));
        assert!(run("execute:\n  echo: false\n", "#| echo: true\n").contains("shown_code"));
    }

    #[test]
    fn the_note_appears_once_after_the_title_block_and_is_escaped() {
        let d = with_outputs(RESULTS, r#"{"note":"Output from <Ada>'s run"}"#).markdown;
        assert_eq!(d.matches("lp-results-note").count(), 1);
        assert!(d.contains("<p class=\"lp-results-note\">Output from &lt;Ada&gt;&#39;s run</p>"));
        assert!(d.find("</header>").unwrap() < d.find("lp-results-note").unwrap());
        let plain = with_outputs("Plain.\n", r#"{"note":"N"}"#).markdown;
        assert!(plain.starts_with("\n<p class=\"lp-results-note\">N</p>"), "{plain}");
        assert_eq!(with_outputs(RESULTS, r#"{"note":""}"#).markdown, compose(RESULTS).markdown);
    }

    #[test]
    fn pandoc_keeps_the_front_matter_and_makes_no_title_block() {
        let front = "---\ntitle: Cited\nbibliography: refs.bib\ncsl: apa.csl\n---\n";
        let source = format!("{front}\nSee @key.\n\n{{{{< include part.qmd >}}}}\n");
        let read = |p: &str| (p == "part.qmd").then(|| "Included words.".to_string());
        let extras = Extras::from_json(r#"{"note":"N"}"#);
        let d = draft(&source, "main.qmd", &read, &extras, true);
        assert!(d.markdown.starts_with(front), "{}", d.markdown);
        assert!(d.markdown.contains("Included words."));
        assert!(!d.markdown.contains("quarto-title-block") && !d.markdown.contains("lp-results-note"));
        assert_eq!(&d.line_map[..5], &[Some(1), Some(2), Some(3), Some(4), Some(5)]);
    }

    #[test]
    fn extras_tolerate_junk() {
        assert!(Extras::from_json("not json").outputs.is_empty());
        let e = Extras::from_json(r#"{"outputs":[{"line":"x"},{"line":3,"html":"h"}],"strings":{"abstract":"Résumé"}}"#);
        assert_eq!(e.outputs.len(), 1);
        assert_eq!(e.strings.abstract_label, "Résumé");
    }

    #[test]
    fn the_draft_json_has_nulls_for_generated_lines() {
        let json = compose("---\ntitle: T\n---\nBody").to_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["line_map"], serde_json::json!([null, 4]));
        assert!(value["markdown"].as_str().unwrap().ends_with("</header>\nBody"));
    }
}
