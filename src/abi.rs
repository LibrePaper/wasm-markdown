//! The sixteen exports a host calls, and nothing else.
//!
//! Each one wraps the shared implementation in `wasm_helpers::abi`.
//! They are written out rather than generated because a `#[no_mangle]` export
//! has to be compiled into the `cdylib` that ships and cannot be inherited from
//! a dependency -- and because sixteen signatures you can read beat a macro
//! that writes them and reports its errors somewhere else.
//!
//! Two of these do nothing here. `set_today` is accepted and ignored, since
//! markdown has no dates, and `set_asset_url` is not: markdown produces HTML a
//! browser will fetch from, so its images are pointed at what the host says is
//! there. A renderer that needs neither still exports both, so that one loader
//! drives every renderer without first asking which it has.

use wasm_helpers::abi;
use wasm_helpers::diagnostic::Compiled;

/// What this module is: markdown, with the host's figures resolved into it.
fn render(source: &str, title: &str) -> Compiled {
    crate::markdown::compile_with(source, title, &|path| abi::asset_url(path))
}

fn heading(source: &str) -> String {
    crate::markdown::title_of(source)
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    abi::alloc(len)
}

/// # Safety
/// `pointer` and `len` must be exactly what a previous `alloc` returned.
#[no_mangle]
pub unsafe extern "C" fn dealloc(pointer: *mut u8, len: usize) {
    abi::dealloc(pointer, len)
}

/// # Safety
/// The pointers and lengths must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn compile(
    source: *const u8,
    source_len: usize,
    title: *const u8,
    title_len: usize,
) -> usize {
    let source = abi::text_at(source, source_len);
    let title = abi::text_at(title, title_len);
    abi::answer_compiled(render(source, title))
}

/// # Safety
/// `source` and `len` must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn title_of(source: *const u8, len: usize) -> usize {
    abi::answer(Ok(heading(abi::text_at(source, len))))
}

/// # Safety
/// `title` and `len` must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn failure_page(title: *const u8, len: usize) -> usize {
    abi::failure_page(title, len)
}

/// # Safety
/// The pointers and lengths must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn word_diff(
    old: *const u8,
    old_len: usize,
    new: *const u8,
    new_len: usize,
) -> usize {
    abi::word_diff(old, old_len, new, new_len)
}

/// # Safety
/// The pointers and lengths must describe memory written into this module.
#[no_mangle]
pub unsafe extern "C" fn add_file(
    path: *const u8,
    path_len: usize,
    body: *const u8,
    body_len: usize,
) {
    abi::add_file(path, path_len, body, body_len)
}

#[no_mangle]
pub extern "C" fn clear_files() {
    abi::clear_files()
}

/// # Safety
/// The pointers and lengths must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn set_asset_url(
    path: *const u8,
    path_len: usize,
    url: *const u8,
    url_len: usize,
) {
    abi::set_asset_url(path, path_len, url, url_len)
}

/// # Safety
/// The pointer and length must describe UTF-8 written into this module.
#[no_mangle]
pub unsafe extern "C" fn set_main(path: *const u8, len: usize) {
    abi::set_main(path, len)
}

/// Accepted and ignored: markdown has no clock and no dates.
#[no_mangle]
pub extern "C" fn set_today(year: i32, month: u32, day: u32) {
    abi::set_today(year, month, day)
}

#[no_mangle]
pub extern "C" fn output_ptr() -> *const u8 {
    abi::output_ptr()
}

#[no_mangle]
pub extern "C" fn ok() -> u32 {
    abi::ok()
}

#[no_mangle]
pub extern "C" fn output_kind() -> u32 {
    abi::output_kind()
}

#[no_mangle]
pub extern "C" fn diagnostics() -> usize {
    abi::diagnostics()
}

#[no_mangle]
pub extern "C" fn diagnostics_ptr() -> *const u8 {
    abi::diagnostics_ptr()
}
