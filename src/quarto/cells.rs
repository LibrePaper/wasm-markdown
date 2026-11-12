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

fn is_off(value: Option<&Yaml>) -> bool {
    matches!(value, Some(Yaml::Bool(false))) || matches!(value, Some(Yaml::Str(s)) if s == "false")
}

/// A document-level code option: the top level of the front matter, then
/// `format:` (`format` is a map of per-format maps; only `html` counts).
fn format_option<'a>(ctx: &Context<'a>, key: &str) -> Option<&'a Yaml> {
    let meta: &'a Map = ctx.meta;
    let nonnull = |v: &'a Yaml| (*v != Yaml::Null).then_some(v);
    if let Some(Yaml::Map(format)) = get(meta, "format") {
        if let Some(Yaml::Map(html)) = get(format, "html") {
            if let Some(v) = get(html, key).and_then(nonnull) {
                return Some(v);
            }
        }
    }
    get(meta, key).and_then(nonnull)
}

/// The code of a cell as a plain code block, as Quarto shows source: no
/// braces on the fence, no `#|` lines; with the file name above it and inside
/// a `<details>` when it folds. The lines of the code keep their source line.
fn show_code(cell: &Cell, ctx: &Context, out: &mut Out) {
    let span = (cell.start, cell.end);
    let code = cell.code.join("\n");
    if code.trim().is_empty() {
        return;
    }
    let fold = option(&cell.options, &[], "code-fold")
        .or_else(|| format_option(ctx, "code-fold"))
        .map(|v| v.text())
        .unwrap_or_default();
    let folded = matches!(fold.as_str(), "true" | "show");
    if folded {
        let summary = option(&cell.options, &[], "code-summary")
            .or_else(|| format_option(ctx, "code-summary"))
            .map(|v| v.text())
            .filter(|t| !t.trim().is_empty())
            // `Strings` is not ours: the default summary is a literal.
            .unwrap_or_else(|| "Code".to_string());
        let open = if fold == "show" { " open" } else { "" };
        out.block(
            &format!(
                "<details class=\"code-fold\"{open}><summary>{}</summary>",
                crate::page::escape(&summary)
            ),
            span,
        );
    }
    if let Some(name) = option(&cell.options, &[], "filename")
        .map(|v| v.text())
        .filter(|t| !t.trim().is_empty())
    {
        out.block(
            &format!(
                "<div class=\"code-filename\">{}</div>",
                crate::page::escape(&name)
            ),
            span,
        );
    }
    let body = cell.end - cell.start - 1;
    let first = cell.start + 1 + body.saturating_sub(cell.code.len());
    let fence = verbatim_fence(&code);
    out.push(&format!("{fence}{}", cell.language), None, span);
    for (n, line) in cell.code.iter().enumerate() {
        out.push(line, Some(first + n), span);
    }
    out.push(&fence, None, span);
    if folded {
        out.block("</details>", span);
    }
}

/// The saved output with its line breaks kept (`<pre>` needs them), and every
/// empty line an HTML comment, so a CommonMark HTML block does not end there.
fn keep_one_block(html: &str) -> String {
    html.replace("\r\n", "\n")
        .split('\n')
        .map(|l| if l.trim().is_empty() { "<!-- -->" } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Puts a cell in the draft, as Quarto shows it: its code (unless `echo` is
/// off) as a plain code block, then the saved output if the host passed one
/// (unless `output` is off or `eval` is false). `include: false` hides all of
/// it. The generated lines belong to the cell, so the editor folds them into it.
pub(crate) fn display(
    cell: &Cell,
    output: Option<&Output>,
    _lines: &[&str],
    ctx: &mut Context,
    out: &mut Out,
) {
    let span = (cell.start, cell.end);
    let opt = |key: &str| option(&cell.options, ctx.options, key);
    if is_off(opt("include")) {
        return;
    }
    let echo = !is_off(opt("echo"));
    let show_output = !is_off(opt("output")) && !is_off(opt("eval"));
    let output = output.filter(|_| show_output);
    let html = output.map(|o| {
        let html = super::outputs::normalize(&o.html, cell, ctx);
        (o, keep_one_block(&html))
    });
    // A labelled cell has exactly one element with its id: the saved float
    // when it carries it, an empty anchor before the code otherwise.
    if !cell.label.is_empty() && ctx.refs.has_target(&cell.label) {
        let carried = html.as_ref().is_some_and(|(_, h)| {
            h.contains(&format!("id=\"{}\"", crate::page::escape(&cell.label)))
        });
        if !carried {
            out.block(
                &format!("<span id=\"{}\"></span>", crate::page::escape(&cell.label)),
                span,
            );
        }
    }
    if echo {
        show_code(cell, ctx, out);
    }
    let Some((output, html)) = html else {
        return;
    };
    let block = if output.stale {
        format!(
            "<div class=\"cell-outputs cell-output-stale\" title=\"{}\">{html}</div>",
            crate::page::escape(&ctx.strings.output_stale)
        )
    } else {
        // A plain container: the outputs inside carry their own classes
        // (cell-output-stdout, -display, …), so it adds no rule of its own.
        format!("<div class=\"cell-outputs\">{html}</div>")
    };
    out.block(&block, span);
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::{compose, no_files, with_outputs, RESULTS};
    use crate::quarto::{draft, Extras};

    #[test]
    fn a_cell_without_output_is_shown_as_plain_source() {
        let d = compose("```{r label=a}\n#| fig-cap: Hi\nshown_one()\n```\n");
        assert_eq!(d.markdown, "```r\nshown_one()\n```\n");
        assert_eq!(d.line_map[1], Some(3), "the code keeps its source line");
        assert_eq!(d.line_map[0], None);
        // Backticks inside the code still cannot close the fence early.
        let d = compose("```{r}\ncat(\"```\")\n```\n").markdown;
        assert!(d.starts_with("~~~r\n"), "{d}");
        // `echo: false` shows nothing.
        let d = compose("```{r,echo=FALSE}\nhidden_one()\n```\n").markdown;
        assert!(!d.contains("hidden_one"), "{d}");
        let d =
            compose("---\nexecute:\n  echo: false\n---\n\n```{r}\nhidden_two()\n```\n").markdown;
        assert!(!d.contains("hidden_two"), "{d}");
        let d = compose(
            "---\nexecute:\n  echo: false\n---\n\n```{r}\n#| echo: true\nback_again()\n```\n",
        )
        .markdown;
        assert!(d.contains("back_again()"), "{d}");
    }

    #[test]
    fn a_filename_goes_above_the_code() {
        for fence in [
            "```{r}\n#| filename: analysis.R\n",
            "```{r filename=analysis.R}\n",
        ] {
            let d = compose(&format!("{fence}x()\n```\n")).markdown;
            assert_eq!(
                d, "\n<div class=\"code-filename\">analysis.R</div>\n\n```r\nx()\n```\n",
                "{fence}"
            );
        }
    }

    #[test]
    fn code_fold_wraps_the_code_in_details() {
        let d = compose("```{r}\n#| code-fold: true\nx()\n```\n").markdown;
        assert!(
            d.contains("<details class=\"code-fold\"><summary>Code</summary>"),
            "{d}"
        );
        assert!(d.find("<details").unwrap() < d.find("```r").unwrap());
        assert!(d.find("```r").unwrap() < d.find("</details>").unwrap());
        let d = compose("```{r}\n#| code-fold: show\n#| code-summary: \"The <code>\"\n#| filename: a.R\nx()\n```\n").markdown;
        assert!(
            d.contains("<details class=\"code-fold\" open><summary>The &lt;code&gt;</summary>"),
            "{d}"
        );
        assert!(d.find("code-filename").unwrap() > d.find("<summary>").unwrap());
        // Document defaults: top level and `format: html:`.
        let d =
            compose("---\ncode-fold: true\ncode-summary: Show\n---\n\n```{r}\nx()\n```\n").markdown;
        assert!(d.contains("<summary>Show</summary>"), "{d}");
        let d = compose("---\nformat:\n  html:\n    code-fold: show\n---\n\n```{r}\nx()\n```\n")
            .markdown;
        assert!(d.contains("<details class=\"code-fold\" open>"), "{d}");
        let d = compose("---\ncode-fold: true\n---\n\n```{r}\n#| code-fold: false\nx()\n```\n")
            .markdown;
        assert!(!d.contains("<details"), "{d}");
        // Every generated line belongs to the cell (line 5 is the empty
        // line after the source's final newline).
        let d = compose("```{r}\n#| code-fold: true\nx()\n```\n");
        assert!(
            d.origin
                .iter()
                .all(|o| *o == (1, 4) || *o == (3, 3) || *o == (5, 5)),
            "{:?}",
            d.origin
        );
    }

    #[test]
    fn include_output_and_eval_options() {
        let source = |opt: &str| format!("```{{r}}\n#| {opt}\nshown_code()\n```\n");
        let run = |opt: &str| {
            with_outputs(
                &source(opt),
                r#"{"outputs":[{"line":1,"html":"<p>OUT</p>"}]}"#,
            )
            .markdown
        };
        let d = run("include: false");
        assert!(!d.contains("shown_code") && !d.contains("OUT"), "{d}");
        assert_eq!(compose(&source("include: false")).markdown, "");
        let d = run("output: false");
        assert!(d.contains("shown_code") && !d.contains("OUT"), "{d}");
        let d = run("eval: false");
        assert!(d.contains("shown_code") && !d.contains("OUT"), "{d}");
        let d = run("echo: false");
        assert!(!d.contains("shown_code") && d.contains("OUT"), "{d}");
        let d = run("code-fold: true");
        assert!(d.contains("<details") && d.contains("OUT"), "{d}");
        assert!(d.find("</details>").unwrap() < d.find("OUT").unwrap());
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
        let one = position("<div class=\"cell-outputs\"><p>ONE</p></div>");
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
            "<div class=\"cell-outputs cell-output-stale\" title=\"Old &lt;one&gt;\"><p>TWO\nlines</p></div>"
        ));
        assert_eq!(d.origin[one], (5, 8), "the output belongs to its cell");
    }

    #[test]
    fn multi_line_output_keeps_its_breaks() {
        let source = "---\ntitle: T\n---\n\n```{r}\nx()\n```\n";
        let json = r#"{"outputs":[{"line":5,"html":"<pre><code>a\n\nb</code></pre>"}]}"#;
        let d = with_outputs(source, json);
        assert!(
            d.markdown.contains(
                "<div class=\"cell-outputs\"><pre><code>a\n<!-- -->\nb</code></pre></div>"
            ),
            "{}",
            d.markdown
        );
    }

    #[test]
    fn format_html_options_win_and_other_formats_are_ignored() {
        let d = compose("---\nformat:\n  html:\n    code-fold: false\n  pdf:\n    code-fold: true\n---\n\n```{r}\nx()\n```\n").markdown;
        assert!(!d.contains("<details"), "{d}");
        let d = compose("---\nformat:\n  pdf:\n    code-fold: true\n---\n\n```{r}\nx()\n```\n")
            .markdown;
        assert!(!d.contains("<details"), "{d}");
    }

    #[test]
    fn a_hidden_cell_gets_no_output_and_is_not_shown() {
        let d = with_outputs(
            RESULTS,
            r#"{"outputs":[{"line":17,"html":"<p>HIDDEN</p>"}]}"#,
        );
        assert!(!d.markdown.contains("HIDDEN"));
        assert!(!d.markdown.contains("c <- 3"));
        let bare = compose(RESULTS).markdown;
        assert!(bare.contains("a <- 1") && bare.contains("b <- 2"));
        assert!(!bare.contains("c <- 3") && !bare.contains("#|") && !bare.contains("{r}"));
        assert!(bare.contains("```r\n"));
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
