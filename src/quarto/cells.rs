//! Executable cells: parsing, options, and how a cell is shown.
//!
//! Owner: fwm8. A cell is shown verbatim, or, when the host passed the output
//! of a run, as its code followed by that output. [`display`] is the cell
//! extension point.

use super::attrs::parse_attributes;
use super::front_matter::{get, scalar, set, Map, Yaml};
use super::{verbatim_fence, Context, Out, Output};

/// The value of an option, looking in the cell first and the defaults second.
/// A null counts as absent, like JavaScript's `??`.
pub(crate) fn option<'a>(
    cell: &'a [(String, Yaml)],
    defaults: &'a [(String, Yaml)],
    key: &str,
) -> Option<&'a Yaml> {
    get(cell, key)
        .filter(|v| **v != Yaml::Null)
        .or_else(|| get(defaults, key).filter(|v| **v != Yaml::Null))
}

pub(crate) fn starts_with_language(inner: &str) -> bool {
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

pub(crate) fn language_of(info: &str) -> String {
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

#[allow(dead_code)] // the feature module reads every field
pub(crate) struct Cell {
    pub start: usize,
    pub end: usize,
    pub language: String,
    pub options: Map,
    pub label: String,
    /// The code, without the `#|` option lines.
    pub code: Vec<String>,
}

pub(crate) struct OpenFence {
    pub ch: u8,
    pub len: usize,
    pub info: String,
    pub start: usize,
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
                while j < chars.len()
                    && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
                {
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

pub(crate) fn build_cell(fence: &OpenFence, lines: &[&str], end: usize) -> Cell {
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
    Cell {
        start: fence.start,
        end,
        language: language_of(&fence.info),
        options: merged,
        label,
        code,
    }
}

/// Puts a cell in the draft: with a saved output, its code (unless `echo` is
/// off) followed by the output; without one, the whole cell as source. The
/// generated lines belong to the cell, so the editor folds them into it.
pub(crate) fn display(
    cell: &Cell,
    output: Option<&Output>,
    lines: &[&str],
    ctx: &mut Context,
    out: &mut Out,
) {
    let span = (cell.start, cell.end);
    match output {
        Some(output) => {
            // Shown as Quarto does: the code without its fence or `#|`
            // lines, unless echo is off, then the output.
            let echo = option(&cell.options, ctx.options, "echo");
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
                    crate::page::escape(&ctx.strings.output_stale)
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
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::{compose, no_files, with_outputs, RESULTS};
    use crate::quarto::{draft, Extras};

    #[test]
    fn a_cell_without_output_is_shown_verbatim_in_a_fence_that_cannot_close_early() {
        let d = compose("```{r,echo=FALSE}\nhidden_one()\n```\n").markdown;
        assert!(d.contains("hidden_one"));
        assert!(d.contains("```{r,echo=FALSE}"));
        assert!(d.starts_with("~~~\n"), "{d}");
    }

    #[test]
    fn saved_output_follows_its_cell() {
        let d = with_outputs(
            RESULTS,
            r#"{"outputs":[{"line":5,"html":"<p>ONE</p>"},{"line":12,"html":"<p>TWO\nlines</p>","stale":true}],
                "strings":{"output_stale":"Old <one>"}}"#,
        );
        let lines: Vec<&str> = d.markdown.split('\n').collect();
        assert!(lines.contains(&"```r"));
        assert!(
            !lines.contains(&"#| label: first"),
            "no option lines when output is shown"
        );
        let position = |text: &str| lines.iter().position(|l| *l == text).unwrap();
        let one = position("<div class=\"lp-cell-output\"><p>ONE</p></div>");
        assert_eq!(
            (lines[one - 1], lines[one + 1]),
            ("", ""),
            "generated HTML is a block of its own"
        );
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
        let d = with_outputs(
            RESULTS,
            r#"{"outputs":[{"line":17,"html":"<p>HIDDEN</p>"}]}"#,
        );
        assert!(!d.markdown.contains("HIDDEN"));
        assert!(d.markdown.contains("#| label: hidden"));
        let bare = compose(RESULTS);
        assert!(bare.markdown.contains("#| label: first"));
        assert!(bare.markdown.contains("{r}"));
    }

    #[test]
    fn echo_decides_whether_the_code_is_shown() {
        let run = |front: &str, opt: &str| {
            let source = format!(
                "---\ntitle: E\n{front}---\n\n```{{r}}\n#| label: only\n{opt}shown_code()\n```\n"
            );
            let line = source.split('\n').position(|l| l == "```{r}").unwrap() + 1;
            let json = format!(r#"{{"outputs":[{{"line":{line},"html":"<p>OUT</p>"}}]}}"#);
            draft(
                &source,
                "e.qmd",
                &no_files,
                &Extras::from_json(&json),
                false,
            )
            .markdown
        };
        assert!(run("", "").contains("shown_code()"));
        let hidden = run("", "#| echo: false\n");
        assert!(!hidden.contains("shown_code"));
        assert!(hidden.contains("OUT"));
        assert!(!run("execute:\n  echo: false\n", "").contains("shown_code"));
        assert!(run("execute:\n  echo: false\n", "#| echo: true\n").contains("shown_code"));
    }
}
