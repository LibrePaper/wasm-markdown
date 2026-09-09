# The module is built for wasm32-unknown-unknown and lands in dist/ under the
# name a host loads it by. `rustup target add wasm32-unknown-unknown` once.
TARGET := target/wasm32-unknown-unknown/release/markdown_wasm.wasm
SOURCES := $(shell find src document.css Cargo.toml -type f 2>/dev/null)

.PHONY: build compress test fmt clean

build: dist/markdown.wasm  ## Build the module

dist/markdown.wasm: $(SOURCES)
	@cargo build --release --target wasm32-unknown-unknown
	@mkdir -p dist
	@cp $(TARGET) $@
	@ls -lh $@ | awk '{print "markdown.wasm", $$5}'

# Compressed once here rather than per request by whatever serves it. Needs
# node, which is the only thing in this repository that does; a build without
# it still produces the module, and a host may compress on the way out
# instead.
compress: dist/markdown.wasm.br  ## Pre-compress the module for serving

dist/markdown.wasm.br: dist/markdown.wasm tools/compress.mjs
	@node tools/compress.mjs $<

test:  ## Run the renderer's tests natively
	@cargo test

fmt:
	@cargo fmt

clean:
	@rm -rf target dist
