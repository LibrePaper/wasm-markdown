//! The front matter: the small YAML reader and the title block.
//!
//! Owner: fwm8. `Yaml`, `Map`, `get`, `set` and `scalar` are read by the other
//! modules (attribute values, cell options); keep their shape.

use crate::diagnostic::Diagnostic;
use crate::page::escape;

use super::{warning, Context};

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

pub(crate) type Map = Vec<(String, Yaml)>;

impl Yaml {
    /// What JavaScript's `String(value)` made of it.
    pub(crate) fn text(&self) -> String {
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

    pub(crate) fn truthy(&self) -> bool {
        match self {
            Yaml::Null => false,
            Yaml::Bool(b) => *b,
            Yaml::Num(n) => *n != 0.0,
            Yaml::Str(s) => !s.is_empty(),
            _ => true,
        }
    }
}

pub(crate) fn get<'a>(map: &'a [(String, Yaml)], key: &str) -> Option<&'a Yaml> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

pub(crate) fn set(map: &mut Map, key: &str, value: Yaml) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key.to_string(), value)),
    }
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

pub(crate) fn scalar(value: &str) -> Yaml {
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
        "author"
            | "authors"
            | "keyword"
            | "keywords"
            | "bibliography"
            | "csl"
            | "filter"
            | "filters"
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
    Some((
        line.len() - t.len(),
        t[..colon].trim(),
        t[colon + 1..].trim_start(),
    ))
}

/// The value of `key` inside the map item `at` of the top-level list `list`.
fn item_slot<'a>(top: &'a mut Map, list: &str, at: usize, key: &str) -> Option<&'a mut Yaml> {
    let Some(Yaml::List(items)) = top.iter_mut().find(|(k, _)| k == list).map(|(_, v)| v) else {
        return None;
    };
    let Some(Yaml::Map(m)) = items.get_mut(at) else {
        return None;
    };
    m.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// The small YAML reader for front matter: top-level scalars, one level of
/// nesting, and lists for the keys that take them. Unknown shapes are warned
/// about; the source itself is never rewritten.
pub fn parse_front_matter(lines: &[&str]) -> FrontMatter {
    let none = |diagnostics: Vec<Diagnostic>| FrontMatter {
        meta: Vec::new(),
        lines: 0,
        diagnostics,
    };
    let first = lines
        .first()
        .map(|l| l.trim_start_matches('\u{feff}').trim());
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
    // A key of the list item that opened a nested list or map (an author's
    // `affiliations:`): its name and its indent.
    let mut nested: Option<(String, usize)> = None;
    for (i, line) in lines.iter().enumerate().take(close).skip(1) {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let (Some((indent, rest)), Some(ak), Some((list, at)), Some((nkey, nindent))) = (
            list_item(line),
            array_key.clone(),
            array_item.clone(),
            nested.clone(),
        ) {
            if indent >= nindent {
                if let Some(slot) = item_slot(&mut top, &list, at, &nkey) {
                    if !matches!(slot, Yaml::List(_)) {
                        *slot = Yaml::List(Vec::new());
                    }
                    if let Yaml::List(entries) = slot {
                        let entry = match rest.find(':') {
                            Some(c) if c > 0 && !rest.starts_with(['"', '\'']) => {
                                let mut m = Map::new();
                                set(&mut m, rest[..c].trim(), scalar(&rest[c + 1..]));
                                Yaml::Map(m)
                            }
                            _ => scalar(rest),
                        };
                        entries.push(entry);
                    }
                }
                let _ = ak;
                continue;
            }
        }
        if let (Some((indent, rest)), Some(ak)) = (list_item(line), array_key.clone()) {
            nested = None;
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
            if let Some(Yaml::List(items)) = top.iter_mut().find(|(k, _)| *k == ak).map(|(_, v)| v)
            {
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
            array_key = if rest.is_empty() && is_array_key(key) {
                Some(key.to_string())
            } else {
                None
            };
            array_item = None;
            array_indent = -1;
            nested = None;
            let value = if !rest.is_empty() {
                scalar(rest)
            } else if array_key.is_some() {
                Yaml::List(Vec::new())
            } else {
                Yaml::Map(Map::new())
            };
            set(&mut top, key, value);
        } else if let (Some((list, at)), true) =
            (array_item.clone(), indent as isize > array_indent)
        {
            let inside = nested.clone().filter(|(_, ni)| indent > *ni);
            if let Some((nkey, _)) = inside {
                if let Some(slot) = item_slot(&mut top, &list, at, &nkey) {
                    match slot {
                        Yaml::List(entries) => {
                            if let Some(Yaml::Map(m)) = entries.last_mut() {
                                set(m, key, scalar(rest));
                            }
                        }
                        Yaml::Map(m) => set(m, key, scalar(rest)),
                        _ => {}
                    }
                }
            } else if let Some(Yaml::List(items)) =
                top.iter_mut().find(|(k, _)| *k == list).map(|(_, v)| v)
            {
                if let Some(Yaml::Map(m)) = items.get_mut(at) {
                    if rest.is_empty() {
                        set(m, key, Yaml::Map(Map::new()));
                        nested = Some((key.to_string(), indent));
                    } else {
                        set(m, key, scalar(rest));
                        nested = None;
                    }
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
            diagnostics.push(warning(
                "quarto_yaml_nesting",
                vec![],
                "",
                i + 1,
                indent + 1,
            ));
        }
    }
    FrontMatter {
        meta: top,
        lines: close + 1,
        diagnostics,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `2026-11-12` as "12 November 2026"; any other text as written. `today`
/// and `last-modified` mean the day the host reported, when it did.
fn format_date(text: &str) -> String {
    let t = text.trim();
    if matches!(t, "today" | "now" | "last-modified") {
        if let Some((y, m, d)) = wasm_helpers::abi::today() {
            if (1..=12).contains(&m) {
                return format!("{d} {} {y}", MONTHS[m as usize - 1]);
            }
        }
        return t.to_string();
    }
    let parts: Vec<&str> = t.split('-').collect();
    if let [y, m, d] = parts[..] {
        if let (4, Ok(m), Ok(d)) = (y.len(), m.parse::<usize>(), d.parse::<u32>()) {
            if y.bytes().all(|b| b.is_ascii_digit()) && (1..=12).contains(&m) && (1..=31).contains(&d)
            {
                return format!("{d} {} {y}", MONTHS[m - 1]);
            }
        }
    }
    t.to_string()
}

/// The names in an `affiliation` value: a string, a map with a `name`, or a
/// list of those.
fn affiliation_names(value: &Yaml) -> Vec<String> {
    match value {
        Yaml::Str(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Yaml::Map(m) => get(m, "name")
            .filter(|v| v.truthy())
            .map(|v| vec![v.text()])
            .unwrap_or_default(),
        Yaml::List(items) => items.iter().flat_map(affiliation_names).collect(),
        _ => Vec::new(),
    }
}

/// The title block of the draft: the HTML that stands for the front matter,
/// or nothing when it has none of title, subtitle, author, date, abstract or
/// keywords. The extension point for the title block (fwm8).
pub(crate) fn title_block(ctx: &Context) -> String {
    let (meta, abstract_label) = (ctx.meta, ctx.strings.abstract_label.as_str());
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
    // Each author's name and the numbers of their affiliations; the numbers
    // follow the first appearance of each distinct affiliation.
    let mut affiliations: Vec<String> = Vec::new();
    let mut people: Vec<(String, Vec<usize>)> = Vec::new();
    for a in &list {
        let (name, names) = match a {
            Yaml::Str(s) => (s.clone(), Vec::new()),
            Yaml::Map(m) => (
                ["name", "literal"]
                    .into_iter()
                    .filter_map(|k| get(m, k).filter(|v| v.truthy()))
                    .map(Yaml::text)
                    .next()
                    .unwrap_or_default(),
                ["affiliations", "affiliation"]
                    .into_iter()
                    .filter_map(|k| get(m, k))
                    .flat_map(affiliation_names)
                    .collect(),
            ),
            _ => (String::new(), Vec::new()),
        };
        if name.is_empty() {
            continue;
        }
        let mut numbers = Vec::new();
        for n in names {
            let at = match affiliations.iter().position(|x| *x == n) {
                Some(at) => at,
                None => {
                    affiliations.push(n);
                    affiliations.len() - 1
                }
            };
            if !numbers.contains(&(at + 1)) {
                numbers.push(at + 1);
            }
        }
        people.push((name, numbers));
    }
    let subtitle = field("subtitle");
    let (title, abstract_text) = (field("title"), field("abstract"));
    let date = format_date(&field("date"));
    let keywords: Vec<String> = match get(meta, "keywords").or_else(|| get(meta, "keyword")) {
        Some(Yaml::List(l)) => l.iter().map(|k| k.text().trim().to_string()).collect(),
        Some(v) if v.truthy() => vec![v.text().trim().to_string()],
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|k| !k.is_empty())
    .collect();
    if title.is_empty()
        && subtitle.is_empty()
        && people.is_empty()
        && date.is_empty()
        && abstract_text.is_empty()
        && keywords.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("<header class=\"title-block\">");
    if !title.is_empty() {
        out.push_str(&format!("<h1 class=\"title\">{}</h1>", escape(&title)));
    }
    if !subtitle.is_empty() {
        out.push_str(&format!("<p class=\"subtitle\">{}</p>", escape(&subtitle)));
    }
    if !people.is_empty() {
        let shown: Vec<String> = people
            .iter()
            .map(|(name, numbers)| {
                let mut s = escape(name);
                if !numbers.is_empty() {
                    let refs: Vec<String> = numbers.iter().map(|n| n.to_string()).collect();
                    s.push_str(&format!(
                        "<sup class=\"affiliation-ref\">{}</sup>",
                        refs.join(",")
                    ));
                }
                s
            })
            .collect();
        out.push_str(&format!("<p class=\"author\">{}</p>", shown.join(", ")));
    }
    if !affiliations.is_empty() {
        let items: Vec<String> = affiliations
            .iter()
            .enumerate()
            .map(|(n, a)| {
                format!(
                    "<span class=\"affiliation\"><sup>{}</sup> {}</span>",
                    n + 1,
                    escape(a)
                )
            })
            .collect();
        out.push_str(&format!("<p class=\"affiliations\">{}</p>", items.join(" ")));
    }
    if !date.is_empty() {
        out.push_str(&format!("<p class=\"date\">{}</p>", escape(&date)));
    }
    if !abstract_text.is_empty() {
        out.push_str(&format!(
            "<div class=\"abstract\"><p class=\"abstract-title\">{}</p><p>{}</p></div>",
            escape(abstract_label),
            escape(&abstract_text)
        ));
    }
    if !keywords.is_empty() {
        out.push_str(&format!(
            "<p class=\"keywords\"><span class=\"keywords-label\">Keywords:</span> {}</p>",
            escape(&keywords.join(", "))
        ));
    }
    out.push_str("</header>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarto::tests::compose;

    #[test]
    fn front_matter_becomes_a_title_block() {
        let source = "---\ntitle: Draft title\nauthors:\n  - name: Ada Lovelace\ndate: 1843\nabstract: A short abstract\n---\n\nBody\n";
        let d = compose(source);
        assert!(
            d.markdown
                .starts_with("<header class=\"title-block\""),
            "{}",
            d.markdown
        );
        assert!(d.markdown.contains("<h1 class=\"title\">Draft title</h1>"));
        assert!(d
            .markdown
            .contains("<p class=\"author\">Ada Lovelace</p>"));
        assert!(d.markdown.contains("<p class=\"date\">1843</p>"));
        assert!(d
            .markdown
            .contains("<div class=\"abstract\"><p class=\"abstract-title\">Abstract</p><p>A short abstract</p></div>"));
        assert!(!d.markdown.contains("\n---\n"));
        // The generated line has no source line; the prose keeps its own.
        assert_eq!(d.line_map[0], None);
        assert_eq!(d.line_map.last(), Some(&Some(10)));
        assert_eq!(
            d.origin[0],
            (1, 7),
            "the title block belongs to the front matter"
        );
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
        let Some(Yaml::List(authors)) = get(&fm.meta, "authors") else {
            panic!()
        };
        assert_eq!(authors.len(), 2);
        let Yaml::Map(first) = &authors[0] else {
            panic!()
        };
        assert_eq!(
            get(first, "affiliation"),
            Some(&Yaml::Str("Analytical Engine".into()))
        );
        assert_eq!(
            get(&fm.meta, "execute"),
            Some(&Yaml::Map(vec![("echo".into(), Yaml::Bool(false))]))
        );

        let unfinished = compose("---\ntitle: Still editing\n\n# Intro\n\nText");
        assert!(unfinished.markdown.contains("Still editing"));
        assert_eq!(unfinished.diagnostics[0].message, "quarto_yaml_unfinished");
        assert_eq!(
            (
                unfinished.diagnostics[0].line,
                unfinished.diagnostics[0].column
            ),
            (1, 1)
        );

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
        assert_eq!(
            scalar("[a, 2]"),
            Yaml::List(vec![Yaml::Str("a".into()), Yaml::Num(2.0)])
        );
        assert_eq!(scalar("éa"), Yaml::Str("éa".into()));
        assert_eq!(Yaml::Num(1843.0).text(), "1843");
    }

    #[test]
    fn nested_authors_and_affiliations_are_read() {
        let lines = [
            "---",
            "author:",
            "  - name: Ada Lovelace",
            "    affiliations:",
            "      - name: Analytical Engine",
            "        city: London",
            "      - Royal Society",
            "    email: ada@x.org",
            "  - name: Grace Hopper",
            "    affiliation:",
            "      name: Navy",
            "title: T",
            "---",
        ];
        let fm = parse_front_matter(&lines);
        assert!(fm.diagnostics.is_empty(), "{:?}", fm.diagnostics);
        let Some(Yaml::List(authors)) = get(&fm.meta, "author") else {
            panic!()
        };
        let Yaml::Map(ada) = &authors[0] else { panic!() };
        let Some(Yaml::List(affs)) = get(ada, "affiliations") else {
            panic!("{ada:?}")
        };
        assert_eq!(affs.len(), 2);
        let Yaml::Map(first) = &affs[0] else { panic!() };
        assert_eq!(get(first, "city"), Some(&Yaml::Str("London".into())));
        assert_eq!(affs[1], Yaml::Str("Royal Society".into()));
        assert_eq!(get(ada, "email"), Some(&Yaml::Str("ada@x.org".into())));
        let Yaml::Map(grace) = &authors[1] else { panic!() };
        let Some(Yaml::Map(navy)) = get(grace, "affiliation") else {
            panic!()
        };
        assert_eq!(get(navy, "name"), Some(&Yaml::Str("Navy".into())));
        assert_eq!(get(&fm.meta, "title"), Some(&Yaml::Str("T".into())));
    }

    #[test]
    fn the_title_block_numbers_distinct_affiliations() {
        let source = "---\ntitle: T\nsubtitle: Sub <b>\nauthor:\n  - name: Ada\n    affiliations:\n      - name: Engine Lab\n      - name: Society\n  - name: Grace\n    affiliation: Engine Lab\n  - Plain Person\ndate: 2026-11-12\nabstract: Abs\nkeywords: [a, b]\n---\n\nBody\n";
        let d = compose(source).markdown;
        assert!(d.contains("<p class=\"subtitle\">Sub &lt;b&gt;</p>"), "{d}");
        assert!(
            d.contains("<p class=\"author\">Ada<sup class=\"affiliation-ref\">1,2</sup>, Grace<sup class=\"affiliation-ref\">1</sup>, Plain Person</p>"),
            "{d}"
        );
        assert!(d.contains(
            "<p class=\"affiliations\"><span class=\"affiliation\"><sup>1</sup> Engine Lab</span> <span class=\"affiliation\"><sup>2</sup> Society</span></p>"
        ));
        assert!(d.contains("<p class=\"date\">12 November 2026</p>"), "{d}");
        assert!(d.contains(
            "<p class=\"keywords\"><span class=\"keywords-label\">Keywords:</span> a, b</p>"
        ));
        // Without affiliations there are no numbers and no list.
        let d = compose("---\ntitle: T\nauthor: Ada\n---\n").markdown;
        assert!(!d.contains("<sup") && !d.contains("affiliations"), "{d}");
    }

    #[test]
    fn dates_are_formatted_or_left_as_written() {
        assert_eq!(format_date("2026-01-05"), "5 January 2026");
        assert_eq!(format_date("Spring 2026"), "Spring 2026");
        assert_eq!(format_date("2026-13-05"), "2026-13-05");
        wasm_helpers::abi::set_today(2027, 3, 9);
        assert_eq!(format_date("today"), "9 March 2027");
        assert_eq!(format_date("last-modified"), "9 March 2027");
    }

    #[test]
    fn a_title_block_may_hold_only_keywords_or_a_subtitle() {
        let d = compose("---\nsubtitle: S\n---\n").markdown;
        assert!(d.contains("<p class=\"subtitle\">S</p>"));
        let d = compose("---\nkeywords:\n  - x\n  - y\n---\n").markdown;
        assert!(d.contains("</span> x, y</p>"), "{d}");
    }
}
