# The module is built for wasm32-unknown-unknown and lands in dist/ under the
# name a host loads it by. `rustup target add wasm32-unknown-unknown` once.
TARGET := target/wasm32-unknown-unknown/release/markdown_wasm.wasm

.PHONY: build test fmt clean

build: dist/markdown.wasm  ## Build the module

dist/markdown.wasm: $(shell find src document.css Cargo.toml -type f 2>/dev/null)
	@cargo build --release --target wasm32-unknown-unknown
	@mkdir -p dist
	@cp $(TARGET) $@
	@ls -lh $@ | awk '{print "markdown.wasm", $$5}'

test:  ## Run the renderer's tests natively
	@cargo test

fmt:
	@cargo fmt

clean:
	@rm -rf target dist
