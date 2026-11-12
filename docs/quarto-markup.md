> Shared markup vocabulary for the Quarto dialect. The canonical copy will live in wasm-helpers as `docs/classes.md`; this copy is the working reference until then. The Quarto modules (`src/quarto/`) emit the markup described here.

# LibrePaper document vocabulary (shared by every format's HTML)

One stylesheet, `wasm-helpers/document.css`, styles every LibrePaper HTML
document: Markdown, Quarto, Org (embedded by the renderers through
`wasm_helpers::page()`), and LaTeX (LaTeXML) and Typst HTML (the browser host
swaps their own stylesheet for this one). Renderers that write their own markup
emit the classes below; foreign markup (LaTeXML `ltx_*`, Typst's HTML) is styled
by adapter sections at the end of the stylesheet, mapping onto the same look.

Design: professional, minimalist, academic. Serif body from a system stack (no
webfonts: pages are self-contained) — Charter, "Bitstream Charter", "Iowan Old
Style", "Source Serif 4", "Source Serif Pro", Georgia, Cambria, serif. Measure
about 68ch, line height 1.6, restrained heading scale in the same serif
(semibold), hairline rules, booktabs tables, small-caps caption labels,
LaTeX-convention theorems, quiet callouts. Monospace: ui-monospace stack.

## Colours: custom properties only
Every colour is a variable on `:root` (light defaults): `--lp-ink`, `--lp-muted`,
`--lp-paper`, `--lp-rule`, `--lp-accent` (links), `--lp-code-bg`, `--lp-code-ink`,
`--lp-mark`, and callout tints `--lp-note`, `--lp-tip`, `--lp-warning`,
`--lp-important`, `--lp-caution`. Dark theme: the same variables redefined under
`.lp-dark` (a class the host puts on `<html>` in the preview frame, or on an
ancestor of the content in the editor). No `prefers-color-scheme` in the sheet:
the host decides. Images keep a white background in dark.

## Layout hooks
- `body` holds the page layout (max-width, margins, paper colour). A host that
  embeds content elsewhere (the editor) neutralises only `body`'s box.

## Title block
```html
<header class="title-block">
  <h1 class="title">…</h1>
  <p class="subtitle">…</p>
  <p class="author">Ada Lovelace<sup class="affiliation-ref">1</sup>, …</p>
  <p class="affiliations"><span class="affiliation"><sup>1</sup> University …</span></p>
  <p class="date">12 November 2026</p>
  <div class="abstract"><p class="abstract-title">Abstract</p><p>…</p></div>
  <p class="keywords"><span class="keywords-label">Keywords:</span> a, b</p>
</header>
```

## Headings, sections, TOC
- `<h2 id="sec-x"><span class="section-number" data-number="1.2"></span>Title</h2>`
- `.unnumbered` on a heading: no number. `<section class="appendix">` wraps appendices (letters A, B).
- `<nav class="toc"><p class="toc-title">Contents</p><ol><li><a href="#…"><span class="section-number">1</span> …</a><ol>…</ol></li></ol></nav>`

## Cross references and footnotes
- `<a class="crossref" href="#fig-x">Figure 1</a>`
- Footnotes as comrak writes them: `sup.footnote-ref > a`, `section.footnotes > ol > li` with `a.footnote-backref`.

## Figures and tables
```html
<figure class="figure" id="fig-x">
  <img src="…" alt="…" style="width:50%">
  <figcaption><span class="caption-label">Figure 1.</span> Caption text</figcaption>
</figure>
<figure class="figure figure-grid" style="--lp-cols:2">…subfigures as figure.subfigure…</figure>
<figure class="table" id="tbl-x">
  <figcaption><span class="caption-label">Table 1.</span> Caption</figcaption>
  <table>…</table>
</figure>
```
All tables are booktabs style: rule above the header, below the header, below the body; no vertical rules; no zebra.

## Equations
```html
<div class="equation" id="eq-x">
  <span data-math-style="display">E = mc^2</span>
  <span class="equation-number">(1)</span>
</div>
```
Number right-aligned, vertically centred. Inline math: `span[data-math-style="inline"]`.

## Theorem family and proofs
```html
<div class="theorem theorem-thm" id="thm-x">
  <p><span class="theorem-label">Theorem 1</span> <span class="theorem-name">(Name)</span>. Body…</p>
</div>
<div class="proof"><p><span class="proof-label">Proof.</span> … </p></div>
```
Kinds: thm lem cor prp cnj (italic body), def exm exr sol rem (upright body).
Label bold; proof label italic; a ∎ at the end of a proof (`.proof > :last-child::after`).

## Callouts
```html
<div class="callout callout-note">
  <div class="callout-title">Note</div>
  <div class="callout-body">…</div>
</div>
<details class="callout callout-tip" open>
  <summary class="callout-title">Tip</summary><div class="callout-body">…</div>
</details>
```
Kinds: note tip warning important caution. Quiet tint + thin left accent.
`callout-simple` (no title bar look), `callout-minimal` (accent only).

## Divs, spans, layout
- Generic `::: {.foo #bar}` → `<div class="foo" id="bar">`; `[t]{.foo}` → `<span class="foo">`.
- `.smallcaps`, `.underline`, `mark` (also `.mark`), `kbd`.
- Columns: `<div class="columns"><div class="column" style="flex-basis:40%">…</div>…</div>`.
- Margin: `<aside class="margin">…</aside>` (floats right into the gutter on wide pages, inline box on narrow).
- Tabset: `<div class="tabset"><div class="tab"><p class="tab-title">R</p>…</div>…</div>` (stacked; a host may enhance into tabs).
- `<div class="pagebreak"></div>` (hidden on screen, page-break in print).
- `<div class="video"><iframe …></iframe></div>` (16:9 responsive).
- Definition lists: `dl > dt, dd`.

## Code
- `pre > code` (muted panel). `<div class="code-filename">analysis.R</div>` directly before a `pre`.
- `<details class="code-fold"><summary>Code</summary><pre>…</pre></details>`.
- Cell output: `<div class="cell-output">…</div>` (`.cell-output-stale` dims it). (The existing `lp-cell-output` classes stay as aliases.)

## Adapters
- LaTeXML: map `ltx_title`, `ltx_authors`/`ltx_personname`/`ltx_role_affiliation`, `ltx_date`, `ltx_abstract`, `ltx_keywords`, `ltx_section`… `ltx_tag` (section numbers), `ltx_figure`/`ltx_table` + `ltx_caption` + `ltx_tag_figure`/`ltx_tag_table`, `ltx_equation`/`ltx_equationgroup` + `ltx_tag_equation`, `ltx_theorem` + `ltx_tag_theorem` / `ltx_font_italic` bodies, `ltx_proof`, `ltx_note`/`ltx_role_footnote`, `ltx_bibliography`/`ltx_bibitem`, `ltx_tabular` (booktabs), `ltx_listing`, `ltx_toclist`.
- Typst HTML: `figure`/`figcaption`, `math`/`math[display=block]` (MathML), headings start at h2 for level 1, `.typst-*` if any.
