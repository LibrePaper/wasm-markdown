//! Relative column widths for pipe tables, the way Pandoc and Quarto set them.
//!
//! comrak drops the dashes of a pipe table's separator line, so the draft
//! carries the widths on its own: an HTML comment line
//! `<!-- lp-colwidths 30.00 70.00 -->` just before the table's header row.
//! `markdown.rs` turns the comment into a `<colgroup>` after comrak ran.
//!
//! Pandoc's rule: a table with a line longer than 72 characters gets widths
//! proportional to the characters in each cell of the separator line. Quarto's
//! `tbl-colwidths` overrides it: `true` (that rule), `false` (never), `auto`
//! (that rule, but not for a table with a link) or a list of percentages. The
//! option is read from the table's caption line, then from the nearest
//! enclosing div, then from the front matter.

use super::attrs::parse_attributes;
use super::divs::div_fence;
use super::front_matter::{get, Yaml};
use super::{Context, Out};

/// The longest line Pandoc lets a table have before it sets widths.
const LONG_LINE: usize = 72;

/// What `tbl-colwidths` asks for.
#[derive(Debug, PartialEq)]
enum Mode {
    True,
    False,
    Auto,
    List(Vec<f64>),
}

fn mode(value: &Yaml) -> Mode {
    match value {
        Yaml::Bool(false) => Mode::False,
        Yaml::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
            "false" => Mode::False,
            "auto" => Mode::Auto,
            t if t.starts_with('[') => numbers(t.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|n| n.trim().parse::<f64>().ok()))
            .map_or(Mode::True, Mode::List),
            _ => Mode::True,
        },
        Yaml::List(items) => numbers(items.iter().map(|y| match y {
            Yaml::Num(n) => Some(*n),
            Yaml::Str(s) => s.trim().parse().ok(),
            _ => None,
        }))
        .map_or(Mode::True, Mode::List),
        _ => Mode::True,
    }
}

/// The values when there is at least one and all of them are numbers.
fn numbers(values: impl Iterator<Item = Option<f64>>) -> Option<Vec<f64>> {
    let all: Option<Vec<f64>> = values.collect();
    all.filter(|v| !v.is_empty())
}

/// Whether `line` is a pipe table's separator line.
fn is_separator(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The lines of the pipe table whose header row is line `start`.
fn table_at(lines: &[&str], start: usize) -> Option<usize> {
    let header = lines.get(start)?;
    if !header.contains('|') || header.trim().is_empty() || !is_separator(lines.get(start + 1)?) {
        return None;
    }
    // A row of a table already open is not a header.
    if start > 0 && lines[start - 1].contains('|') && !lines[start - 1].trim().is_empty() {
        return None;
    }
    let mut end = start + 2;
    while end < lines.len() && lines[end].contains('|') && !lines[end].trim().is_empty() {
        end += 1;
    }
    Some(end)
}

/// The characters between the pipes of each cell of a separator line.
fn separator_cells(line: &str) -> Vec<usize> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().chars().count()).collect()
}

/// The attributes of a `: Caption {...}` line next to the table.
fn caption_attrs(lines: &[&str], start: usize, end: usize) -> Option<Yaml> {
    let near = |i: Option<usize>| i.and_then(|i| lines.get(i)).map(|l| l.trim());
    let above = start.checked_sub(1);
    let below = Some(end);
    let candidates = [
        above,
        above.filter(|&a| lines[a].trim().is_empty()).and_then(|a| a.checked_sub(1)),
        below,
        below.filter(|&b| near(Some(b)) == Some("")).map(|b| b + 1),
    ];
    candidates.into_iter().find_map(|i| {
        let line = near(i)?;
        // A div fence `::: {...}` is not a caption.
        if div_fence(line).is_some() {
            return None;
        }
        let rest = line.strip_prefix(':')?.trim();
        let at = rest.rfind('{').filter(|_| rest.ends_with('}'))?;
        get(&parse_attributes(&rest[at..]).attrs, "tbl-colwidths").cloned()
    })
}

/// `tbl-colwidths` of the nearest enclosing div that sets it.
fn div_attr(lines: &[&str], start: usize) -> Option<Yaml> {
    let mut closed = 0;
    for line in lines[..start].iter().rev() {
        match div_fence(line) {
            Some("") => closed += 1,
            Some(_) if closed > 0 => closed -= 1,
            Some(info) => {
                if let Some(v) = get(&parse_attributes(info).attrs, "tbl-colwidths") {
                    return Some(v.clone());
                }
            }
            None => {}
        }
    }
    None
}

/// The percentages for the table of lines `start..end`, or none for a table
/// the browser should size itself.
fn widths(lines: &[&str], start: usize, end: usize, ctx: &Context) -> Option<Vec<f64>> {
    let setting = caption_attrs(lines, start, end)
        .or_else(|| div_attr(lines, start))
        .or_else(|| get(ctx.meta, "tbl-colwidths").cloned());
    let mode = setting.as_ref().map_or(Mode::True, mode);
    let cells = separator_cells(lines[start + 1]);
    let table = &lines[start..end];
    let raw: Vec<f64> = match mode {
        Mode::False => return None,
        Mode::List(list) => {
            let last = *list.last()?;
            (0..cells.len())
                .map(|n| list.get(n).copied().unwrap_or(last))
                .collect()
        }
        Mode::True | Mode::Auto => {
            let linked = table.iter().any(|l| l.contains("](") || l.contains("<http"));
            let long = table.iter().any(|l| l.chars().count() > LONG_LINE);
            if !long || (mode == Mode::Auto && linked) {
                return None;
            }
            cells.iter().map(|&n| n as f64).collect()
        }
    };
    let total: f64 = raw.iter().sum();
    (total > 0.0).then(|| raw.iter().map(|w| w / total * 100.0).collect())
}

/// Writes the marker line when a pipe table's header row is line `start`.
/// Call it just before that row is written. Nothing for the Pandoc draft,
/// which Pandoc sizes itself.
pub(crate) fn mark(lines: &[&str], start: usize, ctx: &Context, out: &mut Out) {
    if ctx.for_pandoc {
        return;
    }
    let Some(end) = table_at(lines, start) else {
        return;
    };
    let Some(widths) = widths(lines, start, end, ctx) else {
        return;
    };
    let list: Vec<String> = widths.iter().map(|w| format!("{w:.2}")).collect();
    out.push(
        &format!("<!-- lp-colwidths {} -->", list.join(" ")),
        None,
        (start, end - 1),
    );
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::compose;
    use crate::quarto::{draft, Extras};

    const LONG: &str = "| a | b |\n|---|---------|\n| 1 | this cell is long enough to push the whole row past the limit of seventy-two |\n";

    fn marker(source: &str) -> Option<String> {
        compose(source)
            .markdown
            .lines()
            .find(|l| l.starts_with("<!-- lp-colwidths"))
            .map(String::from)
    }

    #[test]
    fn a_short_table_gets_no_marker() {
        assert_eq!(marker("| a | b |\n|---|---------|\n| 1 | 2 |\n"), None);
    }

    #[test]
    fn a_long_line_gives_dash_proportional_widths() {
        let d = compose(LONG).markdown;
        assert!(d.starts_with("<!-- lp-colwidths 25.00 75.00 -->\n| a | b |"), "{d}");
    }

    #[test]
    fn false_suppresses_in_the_front_matter() {
        let source = format!("---\ntbl-colwidths: false\n---\n\n{LONG}");
        assert_eq!(marker(&source), None);
    }

    #[test]
    fn auto_skips_a_table_with_a_link_only() {
        let linked = LONG.replace("| 1 |", "| [x](https://e.test) |");
        assert_eq!(marker(&format!("---\ntbl-colwidths: auto\n---\n\n{linked}")), None);
        assert!(marker(&format!("---\ntbl-colwidths: auto\n---\n\n{LONG}")).is_some());
    }

    #[test]
    fn a_list_forces_widths_on_a_short_table() {
        let source = "---\ntbl-colwidths: [30,70]\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(
            marker(source).as_deref(),
            Some("<!-- lp-colwidths 30.00 70.00 -->")
        );
    }

    #[test]
    fn a_short_list_repeats_its_last_value() {
        let source = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n\n: Cap {tbl-colwidths=\"[20,40]\"}\n";
        assert_eq!(
            marker(source).as_deref(),
            Some("<!-- lp-colwidths 20.00 40.00 40.00 -->")
        );
    }

    #[test]
    fn the_caption_beats_the_div_and_the_front_matter() {
        let source = "---\ntbl-colwidths: false\n---\n\n::: {tbl-colwidths=\"false\"}\n| a | b |\n|---|---|\n| 1 | 2 |\n\n: Cap {tbl-colwidths=\"[30,70]\"}\n:::\n";
        assert_eq!(
            marker(source).as_deref(),
            Some("<!-- lp-colwidths 30.00 70.00 -->")
        );
    }

    #[test]
    fn a_div_beats_the_front_matter() {
        let source = "---\ntbl-colwidths: false\n---\n\n::: {#tbl-x tbl-colwidths=\"[30,70]\"}\n| a | b |\n|---|---|\n| 1 | 2 |\n:::\n";
        assert_eq!(
            marker(source).as_deref(),
            Some("<!-- lp-colwidths 30.00 70.00 -->")
        );
    }

    #[test]
    fn a_captioned_table_has_the_marker_inside_its_figure() {
        let source = format!("{LONG}\n: Caption {{#tbl-x}}\n");
        let d = compose(&source).markdown;
        let figure = d.find("<figure").expect(&d);
        let at = d.find("<!-- lp-colwidths 25.00 75.00 -->\n| a | b |").expect(&d);
        assert!(figure < at, "{d}");
    }

    #[test]
    fn a_table_in_a_code_fence_is_left_alone() {
        assert_eq!(marker(&format!("```\n{LONG}```\n")), None);
    }

    #[test]
    fn the_pandoc_draft_has_no_marker() {
        let d = draft(LONG, "main.qmd", &|_: &str| None, &Extras::default(), true).markdown;
        assert!(!d.contains("lp-colwidths"), "{d}");
    }
}
