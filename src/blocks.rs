//! A document as its top-level blocks, each rendered to HTML.
//!
//! The editor shows a block as rendered output until the cursor enters it, so
//! it needs the HTML of each block and the source lines the block stands for.
//! A block is rendered in the context of the whole document, not on its own:
//! footnote numbers, heading ids and Quarto cross-reference numbers are the
//! ones the full page has.
//!
//! The document is rendered once. Before that, a marker (an HTML comment) is
//! put between the root's children in the syntax tree, and the output is cut
//! at the markers. Rendering each node separately would restart the heading
//! id counter at every node and lose the document-wide ids; cutting finished
//! HTML apart by tag would need an HTML parser. Markers need neither, and the
//! HTML of each block is byte for byte what the page holds.
//!
//! The blocks are found in the *draft*: for Markdown the source itself, for
//! Quarto the draft [`crate::quarto`] composes. Comrak knows the draft line of
//! each node; the draft knows the source range each draft line belongs to,
//! generated lines included. A title block therefore folds into the front
//! matter and a cell's output into the cell.

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use comrak::{parse_document, Arena};

use crate::diagnostic::Diagnostic;
use crate::markdown::{self, Files, Resolve};
use crate::quarto::{self, Draft, Extras};

/// What `compile_blocks` answers: the JSON, and the draft's diagnostics.
pub struct Blocks {
    pub json: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// A block of the draft: its lines there, and its HTML.
struct Rendered {
    from: usize,
    to: usize,
    html: String,
}

/// Renders the top-level blocks of a draft. Footnote definitions, front
/// matter and HTML comments are not blocks: they stay source in the editor.
fn render_draft(markdown: &str, options: &comrak::Options) -> Vec<Rendered> {
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, options);
    markdown::promote_tasks(root);
    let children: Vec<&AstNode> = root.children().collect();
    // The parser moves footnote definitions to the end, and the renderer
    // wraps them in one section. Everything from the first one on is cut.
    let limit = children
        .iter()
        .position(|c| matches!(c.data().value, NodeValue::FootnoteDefinition(_)))
        .unwrap_or(children.len());
    let marker = |n: usize| {
        NodeValue::HtmlBlock(NodeHtmlBlock {
            block_type: 2,
            literal: format!("<!--lp-block-{n}-->"),
        })
    };
    for (n, child) in children.iter().enumerate().take(limit) {
        child.insert_before(arena.alloc(marker(n).into()));
    }
    let end = arena.alloc(marker(limit).into());
    match children.get(limit) {
        Some(child) => child.insert_before(end),
        None => root.append(end),
    }
    let mut html = String::new();
    let _ = markdown::format_html(root, options, &mut html);

    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for n in 0..=limit {
        let text = format!("<!--lp-block-{n}-->");
        let Some(found) = html[at..].find(&text) else {
            break;
        };
        cuts.push((at + found, at + found + text.len()));
        at += found + text.len();
    }
    let mut out = Vec::new();
    // A column-widths marker is a comment block of its own; it goes to the
    // table after it.
    let mut colwidths: Option<String> = None;
    if cuts.len() != limit + 1 {
        return out;
    }
    for (n, child) in children.iter().enumerate().take(limit) {
        let ast = child.data();
        let skipped = match &ast.value {
            NodeValue::FrontMatter(_) => true,
            NodeValue::HtmlBlock(block) => {
                let literal = block.literal.trim();
                literal.starts_with("<!--")
                    && literal.ends_with("-->")
                    && literal.matches("<!--").count() == 1
            }
            _ => false,
        };
        let piece = html[cuts[n].1..cuts[n + 1].0].trim();
        if skipped && piece.starts_with("<!-- lp-colwidths ") {
            colwidths = Some(piece.to_string());
        }
        if skipped || piece.is_empty() {
            continue;
        }
        let piece = match colwidths.take() {
            Some(marker) => markdown::colwidths(&format!("{marker}\n{piece}")),
            None => piece.to_string(),
        };
        let (start, end) = (ast.sourcepos.start.line, ast.sourcepos.end.line);
        out.push(Rendered {
            from: start,
            to: end.max(start),
            html: piece,
        });
    }
    out
}

/// The blocks of a source as JSON, `{"blocks": [{"from_line", "to_line",
/// "html"}]}`, lines 1-based and inclusive in the source. Quarto for a
/// `.qmd` main file, Markdown for everything else.
pub fn compile_blocks(source: &str, main: &str, files: Files, assets: Resolve) -> Blocks {
    let is_quarto = quarto::is_quarto(main);
    let draft = if is_quarto {
        let extras = Extras::load(files);
        quarto::draft(source, main, files, &extras, false)
    } else {
        Draft::identity(source)
    };
    let options = if is_quarto {
        markdown::quarto_options()
    } else {
        markdown::options()
    };
    // (from, to, html) in source lines. Generated lines belong to the block
    // they come from, so two blocks can claim the same lines -- a cell's code
    // and its output -- and a block that begins inside the one before joins it.
    let mut blocks: Vec<(usize, usize, String)> = Vec::new();
    for block in render_draft(&draft.markdown, &options) {
        let last = draft.origin.len();
        let lines = block.from.max(1)..=block.to.min(last);
        let ranges = lines.filter_map(|line| draft.origin.get(line - 1));
        let (from, to) = ranges.fold((usize::MAX, 0), |(a, b), &(x, y)| (a.min(x), b.max(y)));
        if from > to {
            continue;
        }
        let html = markdown::rewrite_images(&block.html, assets);
        match blocks.last_mut() {
            Some(previous) if from <= previous.1 => {
                previous.0 = previous.0.min(from);
                previous.1 = previous.1.max(to);
                previous.2.push('\n');
                previous.2.push_str(&html);
            }
            _ => blocks.push((from, to, html)),
        }
    }
    let list: Vec<serde_json::Value> = blocks
        .into_iter()
        .map(|(from, to, html)| serde_json::json!({ "from_line": from, "to_line": to, "html": html }))
        .collect();
    Blocks {
        json: serde_json::json!({ "blocks": list }).to_string(),
        diagnostics: draft.diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_files(_: &str) -> Option<String> {
        None
    }

    #[derive(Debug)]
    struct Block {
        from: u64,
        to: u64,
        html: String,
    }

    fn run(source: &str, main: &str, files: Files) -> Vec<Block> {
        let blocks = compile_blocks(source, main, files, &markdown::no_assets);
        let value: serde_json::Value = serde_json::from_str(&blocks.json).unwrap();
        value["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| Block {
                from: b["from_line"].as_u64().unwrap(),
                to: b["to_line"].as_u64().unwrap(),
                html: b["html"].as_str().unwrap().to_string(),
            })
            .collect()
    }

    fn lines(blocks: &[Block]) -> Vec<(u64, u64)> {
        blocks.iter().map(|b| (b.from, b.to)).collect()
    }

    #[test]
    fn a_wide_quarto_table_block_has_its_colgroup() {
        let source = "| a | b |\n|---|---------|\n| 1 | this cell is long enough to push the whole row past the limit of seventy-two |\n";
        let blocks = run(source, "main.qmd", &no_files);
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0]
                .html
                .starts_with("<table><colgroup><col style=\"width:25.00%\">"),
            "{}",
            blocks[0].html
        );
        assert!(!blocks[0].html.contains("lp-colwidths"));
    }

    #[test]
    fn markdown_blocks_keep_their_lines() {
        let source = "# Model\n\nA paragraph.\n\n- one\n- two\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n";
        let blocks = run(source, "paper.md", &no_files);
        assert_eq!(lines(&blocks), [(1, 1), (3, 3), (5, 6), (8, 10), (12, 14)]);
        assert!(
            blocks[0].html.starts_with("<h1") && blocks[0].html.contains("id=\"model\""),
            "{}",
            blocks[0].html
        );
        assert!(blocks[1].html.starts_with("<p>A paragraph.</p>"));
        assert!(blocks[2].html.starts_with("<ul>") && blocks[2].html.contains("<li>two</li>"));
        assert!(blocks[3].html.starts_with("<table>"));
        assert!(blocks[4].html.starts_with("<pre"));
        assert!(!blocks.iter().any(|b| b.html.contains("data-sourcepos")));
    }

    #[test]
    fn a_task_list_block_carries_its_state_class() {
        let blocks = run("- [/] doing\n", "paper.md", &no_files);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].html.contains("task-doing"), "{}", blocks[0].html);
    }

    #[test]
    fn footnote_numbers_and_heading_ids_are_the_pages() {
        let source =
            "# A\n\nOne.[^x]\n\n# A\n\nTwo.[^y] and again.[^x]\n\n[^x]: first\n[^y]: second\n";
        let blocks = run(source, "paper.md", &no_files);
        assert_eq!(
            lines(&blocks),
            [(1, 1), (3, 3), (5, 5), (7, 7)],
            "no footnote section: {blocks:?}"
        );
        assert!(
            blocks[1].html.contains("data-footnote-ref>1</a>"),
            "{}",
            blocks[1].html
        );
        assert!(
            blocks[3].html.contains("data-footnote-ref>2</a>"),
            "{}",
            blocks[3].html
        );
        assert!(
            blocks[2].html.contains("id=\"a-1\""),
            "the second heading is a-1: {}",
            blocks[2].html
        );
        assert!(!blocks
            .iter()
            .any(|b| b.html.contains("class=\"footnotes\"")));
    }

    #[test]
    fn front_matter_and_comments_are_not_blocks() {
        let source = "---\ntitle: T\n---\n\n<!-- a note -->\n\nText\n";
        let blocks = run(source, "paper.md", &no_files);
        assert_eq!(lines(&blocks), [(7, 7)]);
    }

    #[test]
    fn a_heading_after_the_front_matter_is_its_own_block() {
        let blocks = run("---\ntitle: T\n---\n# H\n", "paper.qmd", &no_files);
        assert!(
            blocks.iter().any(|b| b.html.starts_with("<h1")),
            "{blocks:?}"
        );
    }

    #[test]
    fn multi_line_output_is_one_block_with_its_breaks() {
        let source = "```{r}\nx()\n```\n";
        let files = |p: &str| {
            (p == ".librepaper/quarto.json").then(|| {
                r#"{"outputs":[{"line":1,"html":"<pre><code>a\n\nb</code></pre>"}]}"#.to_string()
            })
        };
        let blocks = run(source, "paper.qmd", &files);
        let outs: Vec<_> = blocks
            .iter()
            .filter(|b| b.html.contains("cell-outputs"))
            .collect();
        assert_eq!(outs.len(), 1, "{blocks:?}");
        assert!(outs[0].html.contains("<pre><code>a\n"), "{:?}", outs[0]);
        assert!(outs[0].html.contains("b</code></pre>"), "{:?}", outs[0]);
    }

    #[test]
    fn images_are_resolved_like_the_page() {
        let source = "![x](fig/one.png)\n";
        let assets = |p: &str| (p == "fig/one.png").then(|| "https://cdn.test/one.png".to_string());
        let blocks = compile_blocks(source, "paper.md", &no_files, &assets);
        assert!(
            blocks.json.contains("https://cdn.test/one.png"),
            "{}",
            blocks.json
        );
    }

    #[test]
    fn a_quarto_title_block_folds_into_the_front_matter() {
        let source = "---\ntitle: T\nauthor: Ada\n---\n\nSee @fig-x.\n\n```{r}\n#| label: fig-x\nplot(1)\n```\n\n::: {.note}\nHi\n:::\n";
        let blocks = run(source, "paper.qmd", &no_files);
        assert_eq!(
            lines(&blocks),
            [(1, 4), (6, 6), (8, 11), (13, 15)],
            "{blocks:?}"
        );
        assert!(
            blocks[0].html.contains("<h1 class=\"title\">T</h1>"),
            "{}",
            blocks[0].html
        );
        assert!(
            blocks[1]
                .html
                .contains("<a class=\"quarto-crossref crossref\" href=\"#fig-x\">Figure 1</a>"),
            "{}",
            blocks[1].html
        );
        assert!(blocks[2].html.contains("plot(1)"));
        assert!(
            blocks[3].html.starts_with("<div class=\"note\">"),
            "a div is rendered: {}",
            blocks[3].html
        );
        assert!(
            blocks[3].html.contains("<p>Hi</p>") && blocks[3].html.ends_with("</div>"),
            "{}",
            blocks[3].html
        );
    }

    #[test]
    fn a_cells_code_and_output_are_one_block() {
        let source = "# Results\n\n```{r}\n#| label: first\na <- 1\n```\n\nAfter.\n";
        let read = |p: &str| {
            (p == ".librepaper/quarto.json").then(|| {
                r#"{"outputs":[{"line":3,"html":"<p>RESULT</p>"}],"note":"From a run"}"#.to_string()
            })
        };
        let blocks = run(source, "paper.qmd", &read);
        // The note is generated before the first line, with nothing to
        // belong to but the first line, so it joins the heading.
        assert_eq!(lines(&blocks), [(1, 1), (3, 6), (8, 8)], "{blocks:?}");
        assert!(blocks[0].html.contains("lp-results-note"));
        assert!(
            blocks[1].html.contains("a &lt;- 1")
                && blocks[1]
                    .html
                    .contains("<div class=\"cell-outputs\"><p>RESULT</p></div>"),
            "{}",
            blocks[1].html
        );
    }

    #[test]
    fn diagnostics_come_with_the_blocks() {
        let blocks = compile_blocks(
            "::: {.x}\ntext\n",
            "paper.qmd",
            &no_files,
            &markdown::no_assets,
        );
        assert_eq!(blocks.diagnostics[0].message, "quarto_div_unfinished");
    }

    /// The text of the one `<pre><code>` in the cell output of a block, as the
    /// browser shows it: comments dropped, entities decoded.
    fn shown_text(html: &str) -> String {
        let start = html.find("<pre><code>").expect("a pre") + "<pre><code>".len();
        let end = html[start..].find("</code></pre>").expect("its end") + start;
        let mut text = html[start..end].to_string();
        while let Some(a) = text.find("<!--") {
            let b = text[a..].find("-->").unwrap() + a + 3;
            text.replace_range(a..b, "");
        }
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    fn escaped(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn output_text(saved: &str) -> String {
        let source = "```{r}\nx()\n```\n";
        let json = serde_json::json!({"outputs": [{
            "line": 1,
            "html": format!("<pre><code>{}</code></pre>", escaped(saved)),
        }]})
        .to_string();
        let files = move |p: &str| (p == ".librepaper/quarto.json").then(|| json.clone());
        let blocks = run(source, "paper.qmd", &files);
        let out: Vec<_> = blocks
            .iter()
            .filter(|b| b.html.contains("cell-outputs"))
            .collect();
        assert_eq!(out.len(), 1, "{blocks:?}");
        shown_text(&out[0].html)
    }

    #[test]
    fn saved_text_output_is_verbatim_data_frame() {
        let text = "  statistic value\n1      Mean     5\n2 Sample size     4\n";
        assert_eq!(output_text(text), text);
    }

    #[test]
    fn saved_text_output_is_verbatim_summary_lm() {
        let text = "\nCall:\nlm(formula = mpg ~ wt, data = mtcars)\n\nResiduals:\n    Min      1Q  Median      3Q     Max \n-4.5432 -2.3647 -0.1252  1.4096  6.8727 \n\nCoefficients:\n            Estimate Std. Error t value Pr(>|t|)    \n(Intercept)  37.2851     1.8776  19.858  < 2e-16 ***\nwt           -5.3445     0.5591  -9.559 1.29e-10 ***\n---\nSignif. codes:  0 '***' 0.001 '**' 0.01 '*' 0.05 '.' 0.1 ' ' 1\n\nResidual standard error: 3.046 on 30 degrees of freedom\n   \n\n";
        assert_eq!(output_text(text), text);
    }

    #[test]
    fn saved_text_output_is_verbatim_tabs_unicode_long_and_trailing_newline() {
        for text in [
            "a\tb\t\tc\n\tindented\n",
            "\u{3b2}\u{302} = 0.42 \u{b1} 0.05 \u{2014} \u{2713}\n",
            &format!("{}\n", "0123456789".repeat(30)),
            "no trailing newline",
            "one\n\n\ntwo\n\n",
            "  leading spaces\n    more   runs   of   spaces  \n",
        ] {
            assert_eq!(output_text(text), text);
        }
    }
}
