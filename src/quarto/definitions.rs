//! Div-based definition lists, the syntax Quarto 2 uses in place of Pandoc's
//! `Term` / `: Definition` form:
//!
//! ```text
//! ::: {.definition-list}
//! * Term 1
//!   - Definition 1a
//!   - Definition 1b
//! * Term 2
//!   - Definition 2
//! :::
//! ```
//!
//! A div with the `definition-list` class that holds exactly one bullet list
//! of terms, each with a nested bullet list of definitions, becomes a
//! `<dl>`. Any other shape is left alone, as Quarto leaves it. The div's own
//! attributes are dropped, as Quarto drops them. The rewrite runs on the
//! expanded lines before the parse, like `shortcodes::move_contents`.

use super::includes::{Expanded, Origin};
use super::parse;

/// Whether `line` starts a bullet item, and the text after the marker.
fn bullet(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let t = &line[indent..];
    let rest = t
        .strip_prefix("* ")
        .or_else(|| t.strip_prefix("- "))
        .or_else(|| t.strip_prefix("+ "))?;
    Some((indent, rest.trim()))
}

/// The terms and definitions of a div's content lines, each with the index of
/// its first line, or none when the content is not exactly that shape.
fn items(content: &[(usize, &str)]) -> Option<Vec<((usize, String), Vec<(usize, String)>)>> {
    let mut out: Vec<((usize, String), Vec<(usize, String)>)> = Vec::new();
    for &(at, line) in content {
        if line.trim().is_empty() {
            continue;
        }
        match bullet(line) {
            Some((0, text)) => out.push(((at, text.to_string()), Vec::new())),
            Some((_, text)) => out.last_mut()?.1.push((at, text.to_string())),
            None => {
                // A wrapped line belongs to the item above it.
                if !line.starts_with(' ') {
                    return None;
                }
                let item = out.last_mut()?;
                let slot = match item.1.last_mut() {
                    Some(d) => &mut d.1,
                    None => &mut item.0 .1,
                };
                slot.push('\n');
                slot.push_str(line.trim());
            }
        }
    }
    (!out.is_empty() && out.iter().all(|(_, defs)| !defs.is_empty())).then_some(out)
}

pub(crate) fn rewrite(expanded: &mut Expanded) {
    if !expanded.lines.iter().any(|l| l.contains("definition-list")) {
        return;
    }
    let refs: Vec<&str> = expanded.lines.iter().map(String::as_str).collect();
    let parsed = parse(&refs);
    // (first line, last line, replacement lines with the origin they take).
    let mut edits: Vec<(usize, usize, Vec<(String, usize)>)> = Vec::new();
    for div in &parsed.divs {
        let Some(end) = div.end else { continue };
        if !div.classes.iter().any(|c| c == "definition-list") {
            continue;
        }
        let content: Vec<(usize, &str)> = (div.start + 1..end).map(|i| (i, refs[i])).collect();
        if content.iter().any(|(_, l)| super::divs::div_fence(l).is_some()) {
            continue;
        }
        let Some(list) = items(&content) else {
            continue;
        };
        let mut lines: Vec<(String, usize)> = vec![("<dl>".into(), div.start), (String::new(), div.start)];
        for ((term_at, term), defs) in list {
            for (tag, at, text) in std::iter::once(("dt", term_at, term))
                .chain(defs.into_iter().map(|(at, text)| ("dd", at, text)))
            {
                lines.push((format!("<{tag}>"), at));
                lines.push((String::new(), at));
                for part in text.split('\n') {
                    lines.push((part.to_string(), at));
                }
                lines.push((String::new(), at));
                lines.push((format!("</{tag}>"), at));
                lines.push((String::new(), at));
            }
        }
        lines.push(("</dl>".into(), end));
        edits.push((div.start, end, lines));
    }
    if edits.is_empty() {
        return;
    }
    let mut lines: Vec<String> = Vec::with_capacity(expanded.lines.len());
    let mut origin: Vec<Origin> = Vec::with_capacity(expanded.lines.len());
    let mut i = 0;
    while i < expanded.lines.len() {
        if let Some((_, end, replacement)) = edits.iter().find(|(start, _, _)| *start == i) {
            for (text, at) in replacement {
                lines.push(text.clone());
                origin.push(expanded.origin[*at].clone());
            }
            i = end + 1;
        } else {
            lines.push(expanded.lines[i].clone());
            origin.push(expanded.origin[i].clone());
            i += 1;
        }
    }
    expanded.lines = lines;
    expanded.origin = origin;
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::compose;

    #[test]
    fn a_definition_list_div_becomes_a_dl() {
        let d = compose(
            "::: {.definition-list}\n* Term 1\n  - Def **a**\n  - Def b\n* Term 2\n  - Def c\n:::\n",
        )
        .markdown;
        assert!(d.starts_with("<dl>\n"), "{d}");
        assert_eq!(d.matches("<dt>").count(), 2, "{d}");
        assert_eq!(d.matches("<dd>").count(), 3, "{d}");
        assert!(d.contains("Def **a**") && d.trim_end().ends_with("</dl>"), "{d}");
        assert!(!d.contains("definition-list"), "{d}");
    }

    #[test]
    fn a_div_of_another_shape_is_left_alone() {
        let d = compose("::: {.definition-list}\n* Term without definitions\n:::\n").markdown;
        assert!(d.contains("<div class=\"definition-list\">"), "{d}");
    }
}
