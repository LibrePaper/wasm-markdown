# librepaper-wasm-markdown

CommonMark to a standalone HTML page, compiled to WebAssembly.

Part of [LibrePaper](https://github.com/LibrePaper). The module is what a
LibrePaper editor previews a markdown document with, and it is the same code
that renders the page a document is stored as — preview and stored page come
out of one implementation, so they cannot disagree.

The build is about 390 KB, which matters: it travels with the editor shell and
is fetched by everyone who opens a document.

## What it renders

CommonMark plus the GitHub extensions — tables, strikethrough, task lists,
autolinks, footnotes — with smart quotes and dashes, and stable heading ids so
links can point into a document. Raw HTML in the source is kept: a rendered
document is served on its own origin and framed, so the author's HTML is no
more dangerous than the markdown around it.

The page it produces is self-contained: styles inline, no webfonts, no
scripts, nothing to fetch. A published document has to stand on its own.

## Building

```sh
rustup target add wasm32-unknown-unknown   # once
make build                                 # -> dist/markdown.wasm
make test                                  # the renderer's tests, natively
```

Nothing but cargo is needed. There is no bindgen step.

## The interface

Plain WebAssembly exports over linear memory rather than wasm-bindgen, so the
loader on the other side is a few lines of JavaScript and no CLI version has to
match a crate version. The caller allocates, writes UTF-8 into the module's
memory, calls, and reads the result back out.

| Export | What it does |
| --- | --- |
| `alloc` / `dealloc` | reserve and release memory for arguments |
| `compile(source, title)` | render; returns the length of the result |
| `output_ptr` / `ok` / `output_kind` | where the result is, whether it is a document, and its format (1 = HTML) |
| `diagnostics` / `diagnostics_ptr` | what the compiler had to say, as JSON — always `[]` here, since markdown cannot fail |
| `failure_page(title)` | the diagnostics dressed as a document |
| `title_of(source)` | the first level-one heading |
| `add_file` / `clear_files` / `set_main` | the file map a document is compiled against |
| `set_asset_url(path, url)` | where a figure actually lives, for image rewriting |
| `set_today(y, m, d)` | accepted and ignored; markdown has no dates |
| `word_diff(old, new)` | the shared word-level diff, as JSON |

`set_today` and the file map are here because one loader drives every LibrePaper
renderer without first asking which it has. Whether an export does anything is
the module's business, not the host's.

See `src/abi.rs`, which documents the convention in full.

## Keeping in step with the application

`src/text.rs` is vendored from `crates/text/src/lib.rs` in the LibrePaper
application repository. The editor's history panel asks this module for the
same diff the native side computes, so the two must tokenise identically. It is
a verbatim copy — `merge` comes along unused rather than being carved out — so
that a diff against upstream is empty and drift is visible at a glance. Changes
belong upstream first.

## Licence

MIT. See [LICENSE](LICENSE).

Its one dependency, [comrak](https://github.com/kivikakk/comrak), carries its
own licence.
