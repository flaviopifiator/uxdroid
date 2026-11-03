//! `liblog.so`: Android's logging API, written to stderr.
//!
//! bionic's `va_list` is the compiler's `__builtin_va_list`, the System V 24-byte header that glibc
//! uses too, so `__android_log_vprint` passes its argument straight to glibc's `vsnprintf` after the
//! format check that every printf-like shim makes.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use crate::shims::{check_format, die, lossy, vsnprintf, write_stderr};
use crate::table::{Export, Table};

const ANDROID_LOG_DEFAULT: c_int = 1;
const ANDROID_LOG_VERBOSE: c_int = 2;
const ANDROID_LOG_FATAL: c_int = 7;
const LOG_ID_MAIN: i32 = 0;
const LOG_LINE_MAX: usize = 4096;
/// liblog's answer when a message is below the minimum priority.
const EPERM_RESULT: c_int = -libc::EPERM;

/// `struct __android_log_message` from `<android/log.h>`.
#[repr(C)]
struct LogMessage {
    struct_size: usize,
    buffer_id: i32,
    priority: i32,
    tag: *const c_char,
    file: *const c_char,
    line: u32,
    message: *const c_char,
}

type Logger = unsafe extern "C" fn(*const LogMessage);
type Aborter = unsafe extern "C" fn(*const c_char);

static MINIMUM_PRIORITY: AtomicI32 = AtomicI32::new(ANDROID_LOG_DEFAULT);
/// The app's logger and aborter, or 0 for ours.
static LOGGER: AtomicUsize = AtomicUsize::new(0);
static ABORTER: AtomicUsize = AtomicUsize::new(0);
static DEFAULT_TAG: Mutex<Option<CString>> = Mutex::new(None);

fn priority_letter(priority: c_int) -> char {
    match priority {
        2 => 'V',
        3 => 'D',
        4 => 'I',
        5 => 'W',
        6 => 'E',
        7 => 'F',
        _ => '?',
    }
}

/// Writes one line to stderr: the logger every message reaches unless the app installs another.
unsafe extern "C" fn stderr_logger(message: *const LogMessage) {
    if message.is_null() {
        return;
    }
    // SAFETY: liblog's contract makes `message` a valid struct with C strings or nulls.
    let (priority, tag, text) = unsafe {
        let message = &*message;
        (message.priority, lossy(message.tag), lossy(message.message))
    };
    let tag = if tag.is_empty() {
        let default = DEFAULT_TAG.lock().unwrap_or_else(|p| p.into_inner());
        default
            .as_ref()
            .map_or_else(String::new, |t| t.to_string_lossy().into_owned())
    } else {
        tag.into_owned()
    };
    write_stderr(format_args!(
        "{}/{tag}: {text}\n",
        priority_letter(priority)
    ));
}

unsafe extern "C" fn write_log_message(message: *const LogMessage) {
    let logger = LOGGER.load(Ordering::Acquire);
    if logger == 0 {
        // SAFETY: forwards the caller's message under the same contract.
        unsafe { stderr_logger(message) }
    } else {
        // SAFETY: the app installed this function with the logger signature.
        unsafe {
            let logger: Logger = std::mem::transmute(logger);
            logger(message);
        }
    }
}

unsafe extern "C" fn set_logger(logger: Option<Logger>) {
    LOGGER.store(logger.map_or(0, |f| f as usize), Ordering::Release);
}

unsafe extern "C" fn set_aborter(aborter: Option<Aborter>) {
    ABORTER.store(aborter.map_or(0, |f| f as usize), Ordering::Release);
}

unsafe extern "C" fn default_aborter(message: *const c_char) {
    // SAFETY: liblog's contract makes this a C string or null.
    let message = unsafe { lossy(message) };
    die(format_args!("{message}\n"))
}

unsafe extern "C" fn call_aborter(message: *const c_char) {
    let aborter = ABORTER.load(Ordering::Acquire);
    if aborter != 0 {
        // SAFETY: the app installed this function with the aborter signature.
        unsafe {
            let aborter: Aborter = std::mem::transmute(aborter);
            aborter(message);
        }
    }
    // SAFETY: as above. Android aborts even when the app's aborter returns.
    unsafe { default_aborter(message) }
}

unsafe extern "C" fn set_default_tag(tag: *const c_char) {
    let tag = (!tag.is_null())
        // SAFETY: liblog's contract makes this a C string.
        .then(|| unsafe { CStr::from_ptr(tag) }.to_owned());
    *DEFAULT_TAG.lock().unwrap_or_else(|p| p.into_inner()) = tag;
}

extern "C" fn set_minimum_priority(priority: i32) -> i32 {
    MINIMUM_PRIORITY.swap(priority, Ordering::Relaxed)
}

extern "C" fn get_minimum_priority() -> i32 {
    MINIMUM_PRIORITY.load(Ordering::Relaxed)
}

extern "C" fn is_loggable(priority: c_int, _tag: *const c_char, default: c_int) -> c_int {
    let minimum = match MINIMUM_PRIORITY.load(Ordering::Relaxed) {
        ANDROID_LOG_DEFAULT => default,
        minimum => minimum,
    };
    c_int::from(priority >= minimum)
}

extern "C" fn is_loggable_len(
    priority: c_int,
    tag: *const c_char,
    _length: usize,
    default: c_int,
) -> c_int {
    is_loggable(priority, tag, default)
}

unsafe extern "C" fn buf_write(
    buffer: c_int,
    priority: c_int,
    tag: *const c_char,
    message: *const c_char,
) -> c_int {
    if is_loggable(priority, tag, ANDROID_LOG_VERBOSE) == 0 {
        return EPERM_RESULT;
    }
    let message = LogMessage {
        struct_size: size_of::<LogMessage>(),
        buffer_id: buffer,
        priority,
        tag,
        file: std::ptr::null(),
        line: 0,
        message,
    };
    // SAFETY: the message lives across the call, and the strings come from the caller.
    unsafe { write_log_message(&message) };
    1
}

pub(crate) unsafe extern "C" fn write(
    priority: c_int,
    tag: *const c_char,
    message: *const c_char,
) -> c_int {
    // SAFETY: forwards the caller's arguments under the same contract.
    unsafe { buf_write(LOG_ID_MAIN, priority, tag, message) }
}

unsafe extern "C" fn buf_vprint(
    buffer: c_int,
    priority: c_int,
    tag: *const c_char,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: the NDK contract makes `format` a C string.
    unsafe { check_format(format, "__android_log_print") };
    let mut line = [0 as c_char; LOG_LINE_MAX];
    // SAFETY: the buffer size is passed along, and `args` is the caller's valid va_list.
    unsafe {
        vsnprintf(line.as_mut_ptr(), LOG_LINE_MAX, format, args);
        buf_write(buffer, priority, tag, line.as_ptr())
    }
}

unsafe extern "C" fn vprint(
    priority: c_int,
    tag: *const c_char,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    // SAFETY: forwards the caller's arguments under the same contract.
    unsafe { buf_vprint(LOG_ID_MAIN, priority, tag, format, args) }
}

unsafe extern "C" fn vassert(
    condition: *const c_char,
    tag: *const c_char,
    format: *const c_char,
    args: *mut c_void,
) -> ! {
    if format.is_null() {
        // SAFETY: the NDK contract makes these C strings.
        unsafe { write(ANDROID_LOG_FATAL, tag, condition) };
    } else {
        // SAFETY: the NDK contract makes these valid, and `args` is the caller's va_list.
        unsafe { vprint(ANDROID_LOG_FATAL, tag, format, args) };
    }
    // SAFETY: abort takes no arguments and never returns.
    unsafe { libc::abort() }
}

unsafe extern "C" {
    fn uxdroid_android_log_print();
    fn uxdroid_android_log_buf_print();
    fn uxdroid_android_log_assert();
}

variadic_entry!("uxdroid_android_log_print", vprint, "24", "rcx");
variadic_entry!("uxdroid_android_log_buf_print", buf_vprint, "32", "r8");
variadic_entry!("uxdroid_android_log_assert", vassert, "24", "rcx");

pub static TABLE: Table = Table {
    library: "liblog.so",
    exports: &[
        Export::function(
            c"__android_log_assert",
            "void(ptr,ptr,ptr,...)",
            uxdroid_android_log_assert as *const (),
        ),
        Export::function(
            c"__android_log_buf_print",
            "i32(i32,i32,ptr,ptr,...)",
            uxdroid_android_log_buf_print as *const (),
        ),
        Export::function(
            c"__android_log_buf_write",
            "i32(i32,i32,ptr,ptr)",
            buf_write as *const (),
        ),
        Export::function(
            c"__android_log_call_aborter",
            "void(ptr)",
            call_aborter as *const (),
        ),
        Export::function(
            c"__android_log_default_aborter",
            "void(ptr)",
            default_aborter as *const (),
        ),
        Export::function(
            c"__android_log_get_minimum_priority",
            "i32()",
            get_minimum_priority as *const (),
        ),
        Export::function(
            c"__android_log_is_loggable",
            "i32(i32,ptr,i32)",
            is_loggable as *const (),
        ),
        Export::function(
            c"__android_log_is_loggable_len",
            "i32(i32,ptr,u64,i32)",
            is_loggable_len as *const (),
        ),
        // Android's daemon is not there; its logger writes where every other message goes.
        Export::function(
            c"__android_log_logd_logger",
            "void(ptr)",
            stderr_logger as *const (),
        ),
        Export::function(
            c"__android_log_print",
            "i32(i32,ptr,ptr,...)",
            uxdroid_android_log_print as *const (),
        ),
        Export::function(
            c"__android_log_set_aborter",
            "void(ptr)",
            set_aborter as *const (),
        ),
        Export::function(
            c"__android_log_set_default_tag",
            "void(ptr)",
            set_default_tag as *const (),
        ),
        Export::function(
            c"__android_log_set_logger",
            "void(ptr)",
            set_logger as *const (),
        ),
        Export::function(
            c"__android_log_set_minimum_priority",
            "i32(i32)",
            set_minimum_priority as *const (),
        ),
        Export::function(
            c"__android_log_stderr_logger",
            "void(ptr)",
            stderr_logger as *const (),
        ),
        Export::function(
            c"__android_log_vprint",
            "i32(i32,ptr,ptr,va_list)",
            vprint as *const (),
        ),
        Export::function(
            c"__android_log_write",
            "i32(i32,ptr,ptr)",
            write as *const (),
        ),
        Export::function(
            c"__android_log_write_log_message",
            "void(ptr)",
            write_log_message as *const (),
        ),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_valid() {
        crate::table::validate(&TABLE).unwrap();
    }

    #[test]
    fn priorities_below_the_minimum_are_not_loggable() {
        let previous = set_minimum_priority(5);
        assert_eq!(is_loggable(4, std::ptr::null(), ANDROID_LOG_VERBOSE), 0);
        assert_eq!(is_loggable(5, std::ptr::null(), ANDROID_LOG_VERBOSE), 1);
        set_minimum_priority(ANDROID_LOG_DEFAULT);
        assert_eq!(is_loggable(3, std::ptr::null(), 4), 0);
        assert_eq!(is_loggable(3, std::ptr::null(), ANDROID_LOG_VERBOSE), 1);
        set_minimum_priority(previous);
    }
}
