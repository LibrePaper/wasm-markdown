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
//! - fenced divs become `<div>` elements around their rendered Markdown, nested
//!   divs included; shortcodes this renderer does not understand are shown
//!   verbatim, so no half of them is rendered as Markdown;
//! - `{{< include >}}` is expanded from the files the host handed over;
//! - `@fig-x`, `@tbl-x`, `@sec-x`, `@eq-x` and `@lst-x` become numbered links
//!   when the target exists, and stay text when it does not.
//!
//! This is a port of the browser's former JavaScript draft, so that preview,
//! block rendering and publishing share one implementation.
//!
//! # Modules and who owns them
//!
//! This file is the driver: the per-line loop, the draft and its line map, and
//! the [`Context`] every hook receives. The features live in modules of their
//! own, one owner each:
//!
//! - `front_matter.rs` -- the YAML reader and the title block (fwm8);
//! - `cells.rs` -- cell parsing, options, outputs, echo and include (fwm8);
//! - `divs.rs` -- the div tree, div markup and bracketed spans (44vp);
//! - `refs.rs` -- cross-reference numbering, resolution, heading ids (r857);
//! - `shortcodes.rs` -- include expansion and shortcodes by name (g7cv);
//! - `attrs.rs` -- the `{#id .class key=value}` parser, shared (scaffold,
//!   mww4); read it, do not edit it.
//!
//! The rule: a feature worker edits only its own module(s). A change needed in
//! this file or in another module is requested from the coordinator. The
//! extension points are the hooks `front_matter::title_block`,
//! `cells::display`, `divs::{open, close, render_span}`,
//! `refs::{div_open, div_close, decorate_heading}` and
//! `shortcodes::expand`. The markup they emit follows `docs/quarto-markup.md`.

mod attrs;
mod cells;
mod divs;
mod front_matter;
mod refs;
mod shortcodes;

use std::collections::HashMap;

use crate::diagnostic::{Diagnostic, Severity};
use crate::page::escape;

use cells::{build_cell, option, starts_with_language, Cell, OpenFence};
use divs::Div;
use front_matter::{get, Map, Yaml};

pub use front_matter::{parse_front_matter, FrontMatter};
use shortcodes::Site;

/// Says whether a filename is a Quarto document.
pub fn is_quarto(name: &str) -> bool {
    name.to_lowercase().ends_with(".qmd")
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

/// Everything a hook may read, and the sink it may write diagnostics to. The
/// driver builds one per draft; hooks take it so a feature never needs a new
/// parameter from the driver.
pub(crate) struct Context<'a> {
    /// The main file's project path.
    pub path: &'a str,
    /// The top-level front matter.
    pub meta: &'a Map,
    /// The `execute:` defaults of the front matter, for cell options.
    pub options: &'a Map,
    /// Every numbered target of the document, by id.
    pub refs: &'a refs::Refs,
    /// Reads a file the host handed over.
    pub files: &'a dyn Fn(&str) -> Option<String>,
    /// The interface strings the draft prints.
    pub strings: &'a Strings,
    /// Warnings found while writing the draft.
    pub diagnostics: Vec<Diagnostic>,
}

impl Context<'_> {
    /// Adds a warning about `line` (1-based; 0 for none) of `source_name`.
    pub(crate) fn report(&mut self, key: &str, hint: String, source_name: &str, line: usize) {
        let file = if source_name == self.path {
            ""
        } else {
            source_name
        };
        self.diagnostics
            .push(warning(key, vec![hint], file, line, usize::from(line > 0)));
    }
}

// -------------------------------------------------------------- the parse

/// A fence of three or more backticks or tildes: its character, its length
/// and what follows it.
pub(crate) fn fence_open(line: &str) -> Option<(u8, usize, &str)> {
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

pub(crate) fn fence_closes(line: &str, ch: u8, len: usize) -> bool {
    match fence_open(line) {
        Some((c, l, rest)) => c == ch && l >= len && rest.is_empty(),
        None => false,
    }
}

/// What the parse found, before anything is written.
pub(crate) struct Parsed {
    fm: FrontMatter,
    cells: Vec<Cell>,
    divs: Vec<Div>,
    headings: Vec<(usize, String)>,
    diagnostics: Vec<Diagnostic>,
}

pub(crate) fn parse(lines: &[&str]) -> Parsed {
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
                fence = Some(OpenFence {
                    ch,
                    len,
                    info: stripped.to_string(),
                    start: i,
                });
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
        if let Some(id) = refs::heading_id(line) {
            headings.push((i, id));
        }
        if let Some(info) = divs::div_fence(line) {
            if info.is_empty() {
                divs::close_div(&mut divs, &mut stack, i);
            } else {
                divs::open_div(&mut divs, &mut stack, i, info);
            }
        }
        i += 1;
    }
    if let Some(open) = &fence {
        diagnostics.push(warning(
            "quarto_cell_unfinished",
            vec![],
            "",
            open.start + 1,
            1,
        ));
    }
    divs::finish_unclosed(&mut divs, &stack, lines.len());
    if let Some(&top) = stack.last() {
        diagnostics.push(warning(
            "quarto_div_unfinished",
            vec![],
            "",
            divs[top].start + 1,
            1,
        ));
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
    Parsed {
        fm,
        cells,
        divs,
        headings,
        diagnostics,
    }
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
fn prose(line: &str, ctx: &mut Context) -> String {
    let refs = ctx.refs;
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
                out.push_str(&divs::spans(
                    &refs::crossrefs(&line[text_start..i], refs),
                    ctx,
                ));
                out.push_str(&line[i..e]);
                i = e;
                text_start = e;
            }
            None => i += 1,
        }
    }
    out.push_str(&divs::spans(
        &refs::crossrefs(&line[text_start..], refs),
        ctx,
    ));
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
            read(
                "include_unavailable",
                &mut extras.strings.include_unavailable,
            );
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
pub(crate) struct Out {
    lines: Vec<String>,
    map: Vec<Option<usize>>,
    origin: Vec<(usize, usize)>,
}

impl Out {
    /// Adds text, one draft line per line of it. `source` is the 0-based
    /// source line of its first line, if it has one; `origin` is the range
    /// a generated line belongs to.
    pub(crate) fn push(&mut self, text: &str, source: Option<usize>, origin: (usize, usize)) {
        for (n, part) in text.split('\n').enumerate() {
            self.lines.push(part.to_string());
            self.map.push(source.map(|s| s + n));
            self.origin.push(source.map_or(origin, |s| (s + n, s + n)));
        }
    }

    /// Generated HTML with blank lines around it, so it is a block of its own.
    pub(crate) fn block(&mut self, html: &str, origin: (usize, usize)) {
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
pub(crate) fn verbatim_fence(text: &str) -> String {
    let backticks = 3.max(longest_run(text, '`') + 1);
    let tildes = 3.max(longest_run(text, '~') + 1);
    if backticks <= tildes {
        "`".repeat(backticks)
    } else {
        "~".repeat(tildes)
    }
}

/// The HTML line that opens or closes a div: the cross-reference hook for a
/// labelled div, then the generic one.
fn div_html(div: &Div, ctx: &mut Context, opening: bool) -> Option<String> {
    let labelled = if refs::has_prefix(&div.id) {
        if opening {
            refs::div_open(div, ctx)
        } else {
            refs::div_close(div, ctx)
        }
    } else {
        None
    };
    labelled.or_else(|| {
        if opening {
            divs::open(div, ctx)
        } else {
            divs::close(div, ctx)
        }
    })
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
    let references = refs::references(&parsed);
    let mut ctx = Context {
        path,
        meta: &parsed.fm.meta,
        options: &defaults,
        refs: &references,
        files,
        strings: &extras.strings,
        diagnostics: Vec::new(),
    };
    let mut out = Out::default();

    let cell_at: HashMap<usize, &Cell> = parsed.cells.iter().map(|c| (c.start, c)).collect();
    // The divs by the line of their opening and of their closing fence.
    let div_opens: HashMap<usize, usize> = parsed
        .divs
        .iter()
        .enumerate()
        .map(|(n, d)| (d.start, n))
        .collect();
    let div_closes: HashMap<usize, usize> = parsed
        .divs
        .iter()
        .enumerate()
        .filter_map(|(n, d)| d.end.map(|end| (end, n)))
        .collect();
    let last_line = lines.len() - 1;
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
    // The divs open at the current line, innermost last.
    let mut open_divs: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if i < front {
            if for_pandoc {
                out.push(lines[i], Some(i), (i, i));
                if i == front - 1 {
                    out.push("", None, (0, i));
                }
            } else if i == front - 1 {
                let header = front_matter::title_block(&ctx);
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
            let line = lines[i];
            let mapped = if line.contains("{{<") { None } else { Some(i) };
            if let Some((ch, len)) = opaque {
                out.push(line, Some(i), (i, i));
                if fence_closes(line, ch, len) {
                    opaque = None;
                }
            } else if let Some((ch, len, info)) = fence_open(line) {
                // `{=html}` and the other raw blocks (g7cv).
                if !for_pandoc {
                    if let Some(end) = shortcodes::raw_block(&lines, i, (ch, len), info, &mut out) {
                        i = end + 1;
                        continue;
                    }
                }
                opaque = Some((ch, len));
                out.push(line, mapped, (i, i));
            } else if let Some(&n) = div_opens.get(&i) {
                // The fence becomes an HTML line; what follows is content.
                let div = &parsed.divs[n];
                if let Some(html) = div_html(div, &mut ctx, true) {
                    out.block(&html, (div.start, div.end.unwrap_or(last_line)));
                }
                open_divs.push(n);
            } else if let Some(&n) = div_closes.get(&i) {
                let div = &parsed.divs[n];
                if let Some(html) = div_html(div, &mut ctx, false) {
                    out.block(&html, (div.start, i));
                }
                open_divs.pop();
            } else if divs::div_fence(line).is_some() {
                out.push(line, mapped, (i, i));
            } else if shortcodes::has_unsupported_shortcode(line) {
                // Kept whole, so Markdown cannot turn the fragments around
                // it into rendered HTML.
                let fence = verbatim_fence(line);
                out.push(&fence, None, (i, i));
                out.push(line, Some(i), (i, i));
                out.push(&fence, None, (i, i));
            } else {
                let site = Site {
                    depth: 0,
                    seen: &[],
                    source_name: path,
                    line: i + 1,
                };
                let mut expanded = shortcodes::expand_line(line, &mut ctx, &site);
                if refs::is_heading(&expanded) {
                    if let Some(decorated) = refs::decorate_heading(&expanded, &mut ctx) {
                        expanded = decorated;
                    }
                }
                // Included text has no one-to-one source line.
                out.push(&prose(&expanded, &mut ctx), mapped, (i, i));
            }
            i += 1;
            continue;
        };
        cells::display(
            cell,
            outputs.get(&cell.start).copied(),
            &lines,
            &mut ctx,
            &mut out,
        );
        i = cell.end + 1;
    }
    // A div that is never closed is closed at the end of the file.
    while let Some(n) = open_divs.pop() {
        let div = &parsed.divs[n];
        if let Some(html) = div_html(div, &mut ctx, false) {
            out.block(&html, (div.start, last_line));
        }
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
pub(crate) mod tests {
    use super::*;

    pub(crate) fn no_files(_: &str) -> Option<String> {
        None
    }

    pub(crate) fn compose(source: &str) -> Draft {
        draft(source, "main.qmd", &no_files, &Extras::default(), false)
    }

    pub(crate) fn with_files(source: &str, files: &[(&str, &str)]) -> Draft {
        let table: Vec<(String, String)> = files
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let read = move |p: &str| table.iter().find(|(k, _)| k == p).map(|(_, v)| v.clone());
        draft(source, "main.qmd", &read, &Extras::default(), false)
    }

    pub(crate) fn with_outputs(source: &str, json: &str) -> Draft {
        draft(
            source,
            "paper.qmd",
            &no_files,
            &Extras::from_json(json),
            false,
        )
    }

    // Lines: the first cell opens on 5, the second on 12, the hidden one on 17.
    pub(crate) const RESULTS: &str = "---\ntitle: Shared\n---\n\n```{r}\n#| label: first\na <- 1\n```\n\nBetween.\n\n```{r}\n#| label: second\nb <- 2\n```\n\n```{r}\n#| label: hidden\n#| include: false\nc <- 3\n```\n";

    #[test]
    fn detects_quarto_by_extension() {
        assert!(is_quarto("paper.qmd"));
        assert!(is_quarto("A/B.QMD"));
        assert!(!is_quarto("paper.md"));
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
        let d = with_files(
            "```text\n{{< include hidden.qmd >}}\n```\n",
            &[("hidden.qmd", "DO NOT INSERT")],
        );
        assert!(d.markdown.contains("{{< include hidden.qmd >}}"));
        assert!(!d.markdown.contains("DO NOT INSERT"));
    }

    #[test]
    fn cell_and_div_diagnostics() {
        let d = compose("```{r #a}\n1\n```\n\n```{r #a}\n2\n```\n\n```{r}\nopen\n");
        let keys: Vec<&str> = d.diagnostics.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(
            keys,
            [
                "quarto_cell_unfinished",
                "quarto_cell_label_duplicate",
                "quarto_cell_label_duplicate"
            ]
        );
        assert_eq!(d.diagnostics[1].hints, vec!["label=a".to_string()]);
        let d = compose("::: {.x}\ntext\n");
        assert_eq!(d.diagnostics[0].message, "quarto_div_unfinished");
    }

    #[test]
    fn the_note_appears_once_after_the_title_block_and_is_escaped() {
        let d = with_outputs(RESULTS, r#"{"note":"Output from <Ada>'s run"}"#).markdown;
        assert_eq!(d.matches("lp-results-note").count(), 1);
        assert!(d.contains("<p class=\"lp-results-note\">Output from &lt;Ada&gt;&#39;s run</p>"));
        assert!(d.find("</header>").unwrap() < d.find("lp-results-note").unwrap());
        let plain = with_outputs("Plain.\n", r#"{"note":"N"}"#).markdown;
        assert!(
            plain.starts_with("\n<p class=\"lp-results-note\">N</p>"),
            "{plain}"
        );
        assert_eq!(
            with_outputs(RESULTS, r#"{"note":""}"#).markdown,
            compose(RESULTS).markdown
        );
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
        assert!(
            !d.markdown.contains("quarto-title-block") && !d.markdown.contains("lp-results-note")
        );
        assert_eq!(
            &d.line_map[..5],
            &[Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
    }

    #[test]
    fn extras_tolerate_junk() {
        assert!(Extras::from_json("not json").outputs.is_empty());
        let e = Extras::from_json(
            r#"{"outputs":[{"line":"x"},{"line":3,"html":"h"}],"strings":{"abstract":"Résumé"}}"#,
        );
        assert_eq!(e.outputs.len(), 1);
        assert_eq!(e.strings.abstract_label, "Résumé");
    }

    #[test]
    fn the_draft_json_has_nulls_for_generated_lines() {
        let json = compose("---\ntitle: T\n---\nBody").to_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["line_map"], serde_json::json!([null, 4]));
        assert!(value["markdown"]
            .as_str()
            .unwrap()
            .ends_with("</header>\nBody"));
    }
}
