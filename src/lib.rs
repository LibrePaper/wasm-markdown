//! CommonMark to the standalone HTML page a LibrePaper document is stored as.
//!
//! The renderer is compiled to WebAssembly and loaded by an editor, which
//! previews with it, and it is the same code that renders a document when one
//! is published. That is the whole point of the module: preview and stored
//! page come out of one implementation, so they cannot disagree.
//!
//! `markdown.wasm` is a few hundred kilobytes and travels with the editor
//! shell. See `src/abi.rs` for the interface it exposes, which is plain
//! exports over linear memory rather than wasm-bindgen -- a loader for it is a
//! few lines of JavaScript and the build needs nothing but cargo.

pub mod diagnostic;
pub mod markdown;
pub mod page;

/// The word diff, vendored so that this module and the application compute the
/// same one. Only `diff` is reached from here, so a native build of this crate
/// sees the rest as unused.
#[allow(dead_code)]
mod text;

#[cfg(target_arch = "wasm32")]
mod abi;
