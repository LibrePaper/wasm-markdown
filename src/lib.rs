//! CommonMark to the standalone HTML page a LibrePaper document is stored as.
//!
//! The renderer is compiled to WebAssembly and loaded by an editor, which
//! previews with it, and it is the same code that renders a document when one
//! is published. That is the whole point of the module: preview and stored
//! page come out of one implementation, so they cannot disagree.
//!
//! `markdown.wasm` is a few hundred kilobytes and travels with the editor
//! shell. What this repository holds is the renderer; the page template, the
//! diagnostics and the WebAssembly interface it answers through come from
//! `librepaper-wasm-helpers`, whose version is the interface's version.

/// The shared page template and the shape a compile answers in, re-exported so
/// that `crate::page` and `crate::diagnostic` mean here what they mean in every
/// other renderer.
pub use librepaper_wasm_helpers::{diagnostic, page};

pub mod markdown;

#[cfg(target_arch = "wasm32")]
mod abi;
