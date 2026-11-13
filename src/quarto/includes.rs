//! Block-level includes, spliced in before the document is parsed.
//!
//! A line that is only `{{< include file.qmd >}}` stands for the whole file.
//! Expanding it first lets the parse, the numbering and the driver see one
//! document: a callout, a figure or a heading in an included file is a real
//! one. An include in the middle of a line stays with the shortcodes, which
//! expand it after the parse.

use crate::diagnostic::Diagnostic;

use super::front_matter::parse_front_matter;
use super::shortcodes::{block_include, cycle_span, resolve, unavailable_span, MAX_INCLUDE_DEPTH};
use super::{fence_closes, fence_open, warning, Strings};

/// Where a line of the expanded text comes from.
#[derive(Clone)]
pub(crate) struct Origin {
    /// Whether it is in the main file.
    pub main: bool,
    /// The file it is in: the main path, or an included file's normalized name.
    pub file: String,
    /// Its 0-based line there.
    pub line: usize,
    /// The 0-based main-file line it stands at: itself, or the line of the
    /// outermost include that brought it in.
    pub anchor: usize,
    /// The normalized names of the files being included, outermost first.
    pub chain: Vec<String>,
}

/// The source with its block-level includes expanded.
pub(crate) struct Expanded {
    pub lines: Vec<String>,
    /// One per line.
    pub origin: Vec<Origin>,
    /// Includes that could not be expanded, already naming the real file and
    /// line.
    pub diagnostics: Vec<Diagnostic>,
}

struct Walk<'a> {
    files: &'a dyn Fn(&str) -> Option<String>,
    strings: &'a Strings,
    out: Expanded,
}

/// Expands the block-level includes of the main file `path`.
pub(crate) fn expand(
    lines: &[&str],
    path: &str,
    files: &dyn Fn(&str) -> Option<String>,
    strings: &Strings,
) -> Expanded {
    let mut walk = Walk {
        files,
        strings,
        out: Expanded {
            lines: Vec::new(),
            origin: Vec::new(),
            diagnostics: Vec::new(),
        },
    };
    walk.file(lines, path, &[], None);
    walk.out
}

impl Walk<'_> {
    /// Adds the lines of `file`. `anchor` is the main-file line of the include
    /// that brought it in; none for the main file itself.
    fn file(&mut self, lines: &[&str], file: &str, chain: &[String], anchor: Option<usize>) {
        // An included file's own front matter is not part of the text.
        let front = parse_front_matter(lines).lines;
        let mut fence: Option<(u8, usize)> = None;
        for (i, &line) in lines.iter().enumerate() {
            let origin = Origin {
                main: anchor.is_none(),
                file: file.to_string(),
                line: i,
                anchor: anchor.unwrap_or(i),
                chain: chain.to_vec(),
            };
            if i < front {
                if anchor.is_none() {
                    self.push(line, origin);
                }
                continue;
            }
            if let Some((ch, len)) = fence {
                if fence_closes(line, ch, len) {
                    fence = None;
                }
                self.push(line, origin);
                continue;
            }
            if let Some((ch, len, _)) = fence_open(line) {
                fence = Some((ch, len));
                self.push(line, origin);
                continue;
            }
            match block_include(line) {
                Some(name) => self.include(name, origin),
                None => self.push(line, origin),
            }
        }
    }

    fn push(&mut self, line: &str, origin: Origin) {
        self.out.lines.push(line.to_string());
        self.out.origin.push(origin);
    }

    /// Replaces the include line at `at` with the file's lines, or with a note
    /// when it cannot be read.
    fn include(&mut self, name: &str, at: Origin) {
        let shown = if at.main { "" } else { at.file.as_str() };
        let Some((normalized, text)) = resolve(self.files, name, &at.file) else {
            let diagnostic = warning(
                "quarto_include_unauthorized",
                vec![format!("name={name}")],
                shown,
                at.line + 1,
                1,
            );
            self.out.diagnostics.push(diagnostic);
            let note = unavailable_span(self.strings, name);
            return self.push(&note, at);
        };
        if at.chain.len() >= MAX_INCLUDE_DEPTH || at.chain.contains(&normalized) {
            let diagnostic = warning(
                "quarto_include_cycle",
                vec![format!("normalized={normalized}")],
                shown,
                at.line + 1,
                1,
            );
            self.out.diagnostics.push(diagnostic);
            let note = cycle_span(self.strings, &normalized);
            return self.push(&note, at);
        }
        let mut chain = at.chain.clone();
        chain.push(normalized.clone());
        let text: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        self.file(&text, &normalized, &chain, Some(at.anchor));
    }
}

#[cfg(test)]
mod tests {
    use crate::quarto::tests::with_files;
    use crate::quarto::{draft, Extras};

    const CALLOUT: &str = "::: {.callout-note}\nInside the note.\n:::";

    #[test]
    fn an_included_callout_renders_as_a_callout() {
        let d = with_files(
            "Intro.\n\n{{< include note.qmd >}}\n\nOutro.\n",
            &[("note.qmd", CALLOUT)],
        );
        assert!(
            d.markdown.contains("<div class=\"callout callout-note\">"),
            "{}",
            d.markdown
        );
        assert!(!d.markdown.contains(":::"), "{}", d.markdown);
        assert!(d.markdown.contains("Inside the note."));
    }

    #[test]
    fn labels_in_an_included_file_are_numbered_with_the_main_ones() {
        let d = with_files(
            "```{r #fig-a}\n#| fig-cap: A\nplot(1)\n```\n\n{{< include more.qmd >}}\n\nSee @fig-a and @fig-b.\n",
            &[(
                "more.qmd",
                "```{r #fig-b}\n#| fig-cap: B\nplot(2)\n```\n",
            )],
        );
        let m = &d.markdown;
        assert!(m.contains("href=\"#fig-a\">Figure 1</a>"), "{m}");
        assert!(m.contains("href=\"#fig-b\">Figure 2</a>"), "{m}");
    }

    #[test]
    fn an_included_files_front_matter_is_stripped() {
        let d = with_files(
            "{{< include part.qmd >}}\n",
            &[("part.qmd", "---\ntitle: Part\n---\nPart text.")],
        );
        assert!(d.markdown.contains("Part text."), "{}", d.markdown);
        assert!(!d.markdown.contains("title: Part"), "{}", d.markdown);
    }

    #[test]
    fn nested_includes_resolve_relative_to_the_including_file() {
        let d = with_files(
            "{{< include chap/one.qmd >}}",
            &[
                ("chap/one.qmd", "One\n{{< include two.qmd >}}"),
                ("chap/two.qmd", "Two"),
                ("two.qmd", "WRONG"),
            ],
        );
        assert_eq!(d.markdown, "One\nTwo");
    }

    #[test]
    fn cycles_and_missing_files_keep_their_file_and_line() {
        let d = with_files(
            "Text\n{{< include a.qmd >}}",
            &[
                ("a.qmd", "A\n{{< include b.qmd >}}"),
                (
                    "b.qmd",
                    "B\n\n{{< include a.qmd >}}\n{{< include gone.qmd >}}",
                ),
            ],
        );
        let cycle = d
            .diagnostics
            .iter()
            .find(|x| x.message == "quarto_include_cycle")
            .unwrap();
        assert_eq!((cycle.file.as_str(), cycle.line), ("b.qmd", 3));
        assert_eq!(cycle.hints, vec!["normalized=a.qmd".to_string()]);
        let gone = d
            .diagnostics
            .iter()
            .find(|x| x.message == "quarto_include_unauthorized")
            .unwrap();
        assert_eq!((gone.file.as_str(), gone.line), ("b.qmd", 4));
        assert!(d.markdown.contains("Include cycle at a.qmd"));
        assert!(d.markdown.contains("Include gone.qmd is not available"));
    }

    #[test]
    fn the_line_map_skips_included_lines_and_shifts_the_rest() {
        let d = with_files(
            "Before\n{{< include part.qmd >}}\nAfter",
            &[("part.qmd", "P1\nP2\nP3")],
        );
        assert_eq!(d.markdown, "Before\nP1\nP2\nP3\nAfter");
        assert_eq!(d.line_map, [Some(1), None, None, None, Some(3)]);
        // An included line belongs to the include that brought it in.
        assert_eq!(d.origin, [(1, 1), (2, 2), (2, 2), (2, 2), (3, 3)]);
    }

    #[test]
    fn a_saved_output_still_attaches_to_a_cell_after_an_include() {
        let source = "{{< include part.qmd >}}\n\n```{r}\nx <- 1\n```\n";
        let read = |p: &str| (p == "part.qmd").then(|| "a\nb\nc\nd".to_string());
        let extras = Extras::from_json(r#"{"outputs":[{"line":3,"html":"<p>OUT</p>"}]}"#);
        let d = draft(source, "main.qmd", &read, &extras, false);
        assert!(d.markdown.contains("<p>OUT</p>"), "{}", d.markdown);
        // A cell in an included file takes the output that names its file.
        let source = "{{< include part.qmd >}}\n";
        let read = |p: &str| (p == "part.qmd").then(|| "```{r}\nx <- 1\n```".to_string());
        let on = |json: &str| draft(source, "main.qmd", &read, &Extras::from_json(json), false);
        let d = on(r#"{"outputs":[{"line":1,"path":"part.qmd","html":"<p>OUT</p>"}]}"#);
        assert!(d.markdown.contains("<p>OUT</p>"), "{}", d.markdown);
        // No path means the main file, whose line 1 is the include itself.
        let d = on(r#"{"outputs":[{"line":1,"html":"<p>OUT</p>"}]}"#);
        assert!(!d.markdown.contains("<p>OUT</p>"), "{}", d.markdown);
        let d = on(r#"{"outputs":[{"line":1,"path":"other.qmd","html":"<p>OUT</p>"}]}"#);
        assert!(!d.markdown.contains("<p>OUT</p>"), "{}", d.markdown);
    }

    #[test]
    fn a_saved_output_attaches_to_a_cell_in_a_nested_include() {
        let source = "{{< include chap/part.qmd >}}\n";
        let read = |p: &str| (p == "chap/part.qmd").then(|| "Intro\n\n```{r}\nx <- 1\n```".to_string());
        let extras = Extras::from_json(
            r#"{"outputs":[{"line":3,"path":"chap/part.qmd","html":"<p>NESTED</p>"}]}"#,
        );
        let d = draft(source, "main.qmd", &read, &extras, false);
        assert!(d.markdown.contains("<p>NESTED</p>"), "{}", d.markdown);
    }

    #[test]
    fn parse_diagnostics_point_at_the_included_file() {
        let d = with_files(
            "Top\n{{< include part.qmd >}}",
            &[("part.qmd", "ok\n\n```{r}\nunfinished")],
        );
        let x = d
            .diagnostics
            .iter()
            .find(|x| x.message == "quarto_cell_unfinished")
            .unwrap();
        assert_eq!((x.file.as_str(), x.line), ("part.qmd", 3));
        let d = with_files(
            "{{< include part.qmd >}}\n```{r}\nunfinished",
            &[("part.qmd", "ok")],
        );
        let x = &d.diagnostics[0];
        assert_eq!((x.file.as_str(), x.line), ("", 2));
    }

    #[test]
    fn an_include_in_a_code_fence_stays_literal() {
        let d = with_files(
            "```text\n{{< include secret.qmd >}}\n```\n",
            &[("secret.qmd", "SECRET")],
        );
        assert!(d.markdown.contains("{{< include secret.qmd >}}"));
        assert!(!d.markdown.contains("SECRET"));
    }
}
