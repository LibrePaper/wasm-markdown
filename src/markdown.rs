//! Markdown, rendered to the page a document is stored as.
//!
//! Rendering at publish time rather than per request keeps one thing true that
//! everything else depends on: a document is HTML, addressed by the hash of the
//! bytes actually served. The reader anchors comments into those bytes, so they
//! must not change underneath a comment.

use comrak::options::{Extension, Parse, Render};
use comrak::Options;

use crate::page;

/// The one markdown configuration this project has: the command line, the
/// server and the browser build all render through it, so a document reads the
/// same whichever of them produced it.
pub fn options() -> Options<'static> {
    let extension = Extension {
        table: true,
        strikethrough: true,
        tasklist: true,
        autolink: true,
        footnotes: true,
        // `$…$` and `$$…$$` become opaque math, carried as TeX in a tagged
        // span rather than parsed as markdown: `x^*` and `y^*` in one sentence
        // are not emphasis, and `\\` in an aligned block is not a line break.
        // Typesetting is the host's: it knows where its KaTeX lives, and
        // this module does not. A dollar with a number after it is still money.
        math_dollars: true,
        // Stable anchors for links into the document.
        header_id_prefix: Some(String::new()),
        front_matter_delimiter: Some("---".to_string()),
        ..Extension::default()
    };
    // Quotes and dashes, the way a typographer sets them.
    let parse = Parse {
        smart: true,
        ..Parse::default()
    };
    // The document is served on its own origin and framed; raw HTML in the
    // source is the author's own, and no more dangerous than the markdown
    // around it.
    let render = Render {
        r#unsafe: true,
        ..Render::default()
    };
    Options {
        extension,
        parse,
        render,
    }
}

/// The options for a Quarto draft: the markdown configuration plus Pandoc's
/// extras. Plain Markdown never gets these, so its output stays as it was.
/// `^[note]` is an inline footnote, `^x^` and `~x~` are super- and subscript
/// (a double `~~` is still strikethrough), and a term line followed by
/// `: definition` is a definition list.
pub fn quarto_options() -> Options<'static> {
    let mut options = options();
    options.extension.inline_footnotes = true;
    options.extension.description_lists = true;
    options.extension.superscript = true;
    options.extension.subscript = true;
    options
}

/// Says whether a filename is one this renders.
pub fn is_markdown(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// The first level-one heading, which is the obvious title when none was
/// given.
pub fn title_of(source: &str) -> String {
    page::first_heading(source, '#')
}

/// Renders a markdown source to body HTML, with no page around it.
pub fn render_body(source: &str) -> String {
    comrak::markdown_to_html(source, &options())
}

/// Where an image in the document actually lives. A markdown document names
/// its figures by path -- `![](fig/one.png)` -- and a path means nothing to a
/// browser showing a page that was never written to a disk. The host is the
/// only thing that knows where those bytes ended up, so it answers.
///
/// Typst needs none of this: it reads a figure through the same file map it
/// reads an import through and embeds it in the PDF. Markdown's images are
/// HTML that comrak has already produced, so the rewriting happens after it
/// rather than inside it.
pub type Resolve<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Nothing beside the document, which is what a one-file markdown source has
/// always compiled against.
pub fn no_assets(_: &str) -> Option<String> {
    None
}

/// Turns a markdown source into the standalone HTML page a document is stored
/// as.
pub fn render(source: &str, title: &str) -> String {
    render_with(source, title, &no_assets)
}

/// The same, with somewhere for the figures to come from.
pub fn render_with(source: &str, title: &str, assets: Resolve) -> String {
    let body = rewrite_images(&render_body(source), assets);
    page::page(title, "", &body)
}

/// The same for a Quarto draft, with the Quarto options.
fn render_quarto_with(source: &str, title: &str, assets: Resolve) -> String {
    let body = rewrite_images(
        &colwidths(&comrak::markdown_to_html(source, &quarto_options())),
        assets,
    );
    page::page(title, "", &body)
}

/// Turns the draft's `<!-- lp-colwidths A B -->` markers into a `<colgroup>`
/// in the table that follows each. A marker with no table after it is dropped.
pub(crate) fn colwidths(html: &str) -> String {
    const OPEN: &str = "<!-- lp-colwidths ";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let Some(close) = after.find("-->") else {
            rest = &rest[at..];
            break;
        };
        let cols: String = after[..close]
            .split_whitespace()
            .map(|w| format!("<col style=\"width:{w}%\">"))
            .collect();
        rest = &after[close + 3..];
        let tail = rest.trim_start();
        if let Some(table) = tail.strip_prefix("<table>") {
            out.push_str(&format!("<table><colgroup>{cols}</colgroup>"));
            rest = table;
        }
    }
    out.push_str(rest);
    out
}

/// Points every relative `src` at what the host says is there, and leaves
/// everything else alone: an absolute URL is the author's own and a data URL
/// is already the bytes. A path the host has nothing for is left as written,
/// so a figure that has not arrived yet is a broken image rather than a
/// rewritten one pointing nowhere.
pub fn rewrite_images(html: &str, assets: Resolve) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("src=\"") {
        let (before, after) = rest.split_at(at + 5);
        out.push_str(before);
        let Some(end) = after.find('"') else {
            rest = after;
            break;
        };
        let (path, tail) = after.split_at(end);
        // Comrak escapes what it writes into an attribute, so the path here is
        // HTML-escaped. The few entities a path can carry are undone before it
        // is compared with the names the document knows.
        let decoded = path
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'");
        let external = decoded.contains("://")
            || decoded.starts_with("data:")
            || decoded.starts_with("blob:")
            || decoded.starts_with('/')
            || decoded.starts_with('#');
        // Comrak percent-encodes a markdown image destination before this
        // function ever sees it -- `fig/my plot.png` arrives as
        // `fig/my%20plot.png` -- but the document's asset map is keyed by the
        // actual filename on disk. Only the path component is decoded, and
        // only for a destination this function is about to hand to the
        // resolver: a query or fragment is not part of a filename and stays
        // as written, and a destination that fails to decode as UTF-8 is
        // looked up by its raw, still-encoded spelling rather than dropped.
        let lookup = if external {
            decoded.clone()
        } else {
            let (path_only, _query_or_fragment) = split_path_query_fragment(&decoded);
            percent_decode_path(path_only)
        };
        match (external, assets(&lookup)) {
            (false, Some(url)) => out.push_str(&crate::page::escape(&url)),
            _ => out.push_str(path),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Splits a URL-shaped string at its first `?` or `#`, so the path can be
/// decoded without touching a query string or fragment that happens to
/// follow it.
fn split_path_query_fragment(s: &str) -> (&str, &str) {
    match s.find(['?', '#']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// A small, dependency-free percent-decoder for the path component of a
/// markdown image destination. This crate builds for `wasm32-unknown-unknown`
/// as well as natively, so a decoder here stays plain byte arithmetic rather
/// than reaching for a crate that may not carry the same feature set on both
/// targets. A destination that decodes to invalid UTF-8 is returned
/// unchanged: a raw, still-encoded lookup that fails is no worse than the bug
/// this is fixing, and it is never worse than a panic.
fn percent_decode_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// One hex nibble, or nothing if the byte is not one.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The same, in the shape every renderer answers in. Comrak has no failure
/// mode, so the list is always empty and every surface built on diagnostics is
/// inert for markdown.
pub fn compile(source: &str, title: &str) -> crate::diagnostic::Compiled {
    compile_with(source, title, &no_assets)
}

pub fn compile_with(source: &str, title: &str, assets: Resolve) -> crate::diagnostic::Compiled {
    crate::diagnostic::Compiled::page(render_with(source, title, assets))
}

/// The text files the host handed the module, by project path. A Quarto
/// document reads its includes and the saved results of its cells from here.
pub type Files<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A Quarto source as the page the preview shows: its draft, rendered like
/// any other markdown, with what the draft had to say attached.
pub fn compile_quarto(
    source: &str,
    title: &str,
    path: &str,
    files: Files,
    assets: Resolve,
) -> crate::diagnostic::Compiled {
    let extras = crate::quarto::Extras::load(files);
    let draft = crate::quarto::draft(source, path, files, &extras, false);
    let mut compiled =
        crate::diagnostic::Compiled::page(render_quarto_with(&draft.markdown, title, assets));
    compiled.diagnostics = draft.diagnostics;
    compiled
}

/// Compiles a source in the dialect its main file's name selects: Quarto for
/// `.qmd`, plain markdown for everything else.
pub fn compile_for(
    source: &str,
    title: &str,
    main: &str,
    files: Files,
    assets: Resolve,
) -> crate::diagnostic::Compiled {
    if crate::quarto::is_quarto(main) {
        compile_quarto(source, title, main, files, assets)
    } else {
        compile_with(source, title, assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_whole_document() {
        let source =
            "# A Paper\n\nSome *emphasis*, a [link](https://example.test) and a footnote.[^1]\n\n\
                      | a | b |\n| - | - |\n| 1 | 2 |\n\n~~struck~~ and `code`\n\n[^1]: the note\n";
        let out = render(source, "A Paper");
        for wanted in [
            "<!doctype html>",
            "<title>A Paper</title>",
            "id=\"a-paper\"", // heading ids, for links into the document
            "<em>emphasis</em>",
            "<a href=\"https://example.test\">link</a>",
            "<table>",
            "<del>",
            "<code>",
            "footnote",
        ] {
            assert!(
                out.contains(wanted),
                "the rendered document is missing {wanted:?}"
            );
        }
        assert!(!out.contains("<script"));
    }

    #[test]
    fn plain_markdown_ignores_the_quarto_extras() {
        let source = "H~2~O, x^2^ and a note^[inline] and ~~struck~~.\n";
        let html = render_body(source);
        for absent in ["<sub>", "<sup", "footnote"] {
            assert!(!html.contains(absent), "{absent} in {html}");
        }
        assert!(html.contains("<del>struck</del>"), "{html}");
        assert!(html.contains("^[inline]"), "{html}");
    }

    #[test]
    fn quarto_options_enable_the_pandoc_extras() {
        let html = comrak::markdown_to_html(
            "H~2~O, x^2^, ~~struck~~ and a note^[inline].\n\nTerm\n: Meaning\n",
            &quarto_options(),
        );
        for wanted in [
            "H<sub>2</sub>O",
            "x<sup>2</sup>",
            "<del>struck</del>",
            "footnote-ref",
            "<dl>",
            "<dt>Term</dt>",
        ] {
            assert!(html.contains(wanted), "{wanted} missing in {html}");
        }
    }

    fn quarto_html(source: &str) -> String {
        let draft = crate::quarto::tests::compose(source).markdown;
        colwidths(&comrak::markdown_to_html(&draft, &quarto_options()))
    }

    const LONG_TABLE: &str = "| a | b |\n|---|---------|\n| 1 | this cell is long enough to push the whole row past the limit of seventy-two |\n";

    #[test]
    fn a_short_quarto_table_has_no_colgroup() {
        let html = quarto_html("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<table>") && !html.contains("colgroup"), "{html}");
        assert!(!html.contains("lp-colwidths"), "{html}");
    }

    #[test]
    fn a_long_quarto_table_gets_proportional_columns() {
        let html = quarto_html(LONG_TABLE);
        assert!(
            html.contains("<table><colgroup><col style=\"width:25.00%\"><col style=\"width:75.00%\"></colgroup>"),
            "{html}"
        );
        assert!(!html.contains("lp-colwidths"), "{html}");
    }

    #[test]
    fn a_captioned_quarto_table_keeps_its_figure_and_gets_columns() {
        let html = quarto_html(&format!("{LONG_TABLE}\n: Caption {{#tbl-x}}\n"));
        let figure = html.find("<figure").expect(&html);
        let cols = html.find("<colgroup>").expect(&html);
        assert!(figure < cols && html.contains("</figure>"), "{html}");
    }

    #[test]
    fn a_marker_without_a_table_is_dropped() {
        assert_eq!(colwidths("<p>x</p>\n<!-- lp-colwidths 50.00 50.00 -->\n<p>y</p>"), "<p>x</p>\n\n<p>y</p>");
    }

    #[test]
    fn plain_markdown_tables_get_no_colgroup() {
        let html = render_body(LONG_TABLE);
        assert!(!html.contains("colgroup"), "{html}");
    }

    #[test]
    fn detects_markdown_by_extension() {
        for (name, want) in [
            ("paper.md", true),
            ("PAPER.MD", true),
            ("notes.markdown", true),
            ("paper.html", false),
            ("md", false),
            ("paper.md.html", false),
        ] {
            assert_eq!(is_markdown(name), want, "{name}");
        }
    }

    #[test]
    fn math_is_carried_as_tex_for_the_host_to_typeset() {
        let out = render_body("Let $x^*$ and $y_1^*$ be optimal, and\n\n$$\n\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}\n$$\n");
        assert!(
            out.contains("<span data-math-style=\"inline\">x^*</span>"),
            "inline math was not kept as TeX: {out}"
        );
        assert!(
            out.contains("<span data-math-style=\"display\">"),
            "display math was not kept as TeX: {out}"
        );
        // What comrak would have made of the same characters without the flag.
        assert!(!out.contains("<em>"), "the stars became emphasis: {out}");
        assert!(
            !out.contains("<br"),
            "the aligned break became a line break: {out}"
        );
        assert!(
            out.contains("&amp;="),
            "the TeX inside the span is not escaped: {out}"
        );
    }

    #[test]
    fn a_dollar_amount_is_not_math() {
        let out = render_body("It costs $5 today and $10 tomorrow.\n");
        assert!(!out.contains("data-math-style"), "{out}");
        assert!(out.contains("$5 today and $10"), "{out}");
    }

    fn no_files(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn markdown_output_is_the_same_through_the_dialect_router() {
        let source = "# A Paper\n\nSome *emphasis* and a note.[^1]\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n[^1]: the note\n";
        let direct = render(source, "A Paper");
        for main in ["paper.md", "", "paper.markdown"] {
            let routed = compile_for(source, "A Paper", main, &no_files, &no_assets);
            assert!(routed.diagnostics.is_empty());
            assert_eq!(routed.into_result().unwrap(), direct, "{main}");
        }
    }

    #[test]
    fn a_qmd_compiles_its_draft_and_reports_what_it_found() {
        let source = "---\ntitle: Paper\n---\n\nSee @fig-a.\n\n```{r}\n#| label: fig-a\nplot(1)\n```\n\n```{r}\nopen\n";
        let out = compile_for(source, "Paper", "paper.qmd", &no_files, &no_assets);
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.diagnostics[0].message, "quarto_cell_unfinished");
        let page = out.into_result().unwrap();
        assert!(page.contains("<h1 class=\"title\">Paper</h1>"), "{page}");
        assert!(page.contains("Figure 1"), "{page}");
    }

    #[test]
    fn raw_html_is_kept() {
        assert!(render_body("<div class=\"x\">hi</div>\n").contains("<div class=\"x\">"));
    }
}
