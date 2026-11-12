//! Cross-references: numbering, resolution, heading ids.
//!
//! Owner: r857. [`references`] numbers every labelled target in document
//! order, [`crossrefs`] turns `@fig-x` into a link, and the hooks below let
//! the owner wrap a labelled div ([`div_open`], [`div_close`]) and decorate a
//! heading ([`decorate_heading`]) without touching the driver.

use std::collections::HashMap;

use crate::page::escape;

use super::divs::Div;
use super::{Context, Parsed};

/// `## Heading {#sec-id}` gives the id.
pub(crate) fn heading_id(line: &str) -> Option<String> {
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

// ------------------------------------------------------------- references

pub(crate) type Refs = HashMap<String, (&'static str, usize)>;

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
pub(crate) fn references(parsed: &Parsed) -> Refs {
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
pub(crate) fn crossrefs(text: &str, refs: &Refs) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('@') {
        let start = i + at;
        i = start + 1;
        let Some(kind) = KINDS.into_iter().find(|k| {
            text[start + 1..]
                .strip_prefix(k)
                .is_some_and(|rest| rest.starts_with('-'))
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

/// Id prefixes that mark a div as a numbered target: figures, tables,
/// sections, equations, listings and the theorem family.
const DIV_PREFIXES: [&str; 15] = [
    "fig", "tbl", "sec", "eq", "lst", "thm", "lem", "cor", "prp", "cnj", "def", "exm", "exr",
    "sol", "rem",
];

/// Whether a div id starts with a cross-reference prefix (`fig-`, `thm-`...).
pub(crate) fn has_prefix(id: &str) -> bool {
    DIV_PREFIXES
        .into_iter()
        .any(|p| id.strip_prefix(p).is_some_and(|rest| rest.starts_with('-')))
}

/// The opening HTML for a div with a cross-reference prefix, or none to let
/// `divs::open` render it. Must answer together with [`div_close`].
pub(crate) fn div_open(_div: &Div, _ctx: &mut Context) -> Option<String> {
    None
}

/// The closing HTML that pairs with [`div_open`].
pub(crate) fn div_close(_div: &Div, _ctx: &mut Context) -> Option<String> {
    None
}

/// Whether a line is an ATX heading (`#` to `######` and a space or nothing).
pub(crate) fn is_heading(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return false;
    }
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(char::is_whitespace)
}

/// The replacement for a heading line (section numbers, anchors), or none to
/// leave it. Called for each heading line of the main text, before
/// references are resolved in it.
pub(crate) fn decorate_heading(_line: &str, _ctx: &mut Context) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::compose;

    #[test]
    fn cross_references_are_numbered_in_document_order() {
        let source = "::: {.callout-note #intro}\nSee @fig-trend.\n:::\n\n\
                      ```{r #fig-trend}\n#| fig-cap: Trend\nplot(1)\n```\n\n\
                      See @fig-trend and @tbl-one and @fig-nope, `@fig-trend` and [@fig-trend](x).\n\n\
                      ::: {#tbl-one}\nTable caption\n:::\n";
        let d = compose(source).markdown;
        assert!(
            d.contains("<div class=\"callout callout-note\" id=\"intro\">"),
            "{d}"
        );
        assert!(d.contains("<div id=\"tbl-one\">"), "{d}");
        assert!(
            d.contains("See <a class=\"quarto-crossref\" href=\"#fig-trend\">Figure 1</a>.\n"),
            "a div's text is resolved too: {d}"
        );
        assert!(
            d.contains("<a class=\"quarto-crossref\" href=\"#fig-trend\">Figure 1</a> and "),
            "{d}"
        );
        assert!(d.contains("href=\"#tbl-one\">Table 1</a>"), "{d}");
        assert!(d.contains("@fig-nope"), "unresolved stays text: {d}");
        assert!(d.contains("`@fig-trend`"), "inline code is untouched: {d}");
        assert!(d.contains("[@fig-trend](x)"), "links are untouched: {d}");
    }
}
