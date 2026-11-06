//! libandroid's `ATrace_*`, which write to the kernel's trace buffer for systrace and Perfetto.
//! uxdroid has no tracing service, so tracing is never enabled, and every call returns at once, as
//! on a device where no trace is being captured.

use std::ffi::c_char;

pub(crate) extern "C" fn begin_section(_name: *const c_char) {}

pub(crate) extern "C" fn end_section() {}

pub(crate) extern "C" fn is_enabled() -> bool {
    false
}

pub(crate) extern "C" fn begin_async_section(_name: *const c_char, _cookie: i32) {}

pub(crate) extern "C" fn end_async_section(_name: *const c_char, _cookie: i32) {}

pub(crate) extern "C" fn set_counter(_name: *const c_char, _value: i64) {}
