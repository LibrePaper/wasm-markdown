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

## Serving it

`make compress` writes `dist/markdown.wasm.br` and `dist/markdown.wasm.gz`
beside the module. A module is compressed once and fetched many times, so
doing it here beats doing it per request at whatever quality a server can
afford inside one:

| | Size | Saving |
| --- | ---: | ---: |
| raw | 388 KiB | |
| brotli, q11 | **105 KiB** | 72.9% |
| gzip, level 9 | 130 KiB | 66.5% |

Brotli quality 11 with a 16 MB window (`lgwin` 24) is the most the format
allows a browser to decode — above 24 is brotli's large-window extension, which
no browser implements. Compressing this module takes about half a second.

Serve the precompressed file with `Content-Encoding: br`, `Vary:
Accept-Encoding`, and — this is the one that bites —
`Content-Type: application/wasm`. `WebAssembly.instantiateStreaming` rejects
anything else, and the content type describes the module, not the encoding it
arrived in.

The compression step is the only thing in this repository that needs node. It
uses `node:zlib`, so there is nothing to install, and a build without node
still produces the module.

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

## Keeping in step

The page template, the diagnostics type, the word diff and the WebAssembly
interface come from
[librepaper-wasm-helpers](https://github.com/LibrePaper/librepaper-wasm-helpers),
whose version is the interface's version: if a host has to be called
differently, that crate changes and this one fails to compile until it is
rebuilt.

The word diff inside it is vendored from the application repository, because
the editor's history panel asks this module for the same diff the native side
computes. Changes to it belong upstream first.

## Licence

MIT. See [LICENSE](LICENSE).

Its one dependency, [comrak](https://github.com/kivikakk/comrak), carries its
own licence.
