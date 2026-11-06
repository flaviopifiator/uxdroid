//! `libz.so`, which Android ships as a system library and the NDK lets apps link against.
//!
//! zlib's ABI depends on no libc type: `z_stream`, `gz_header` and the callbacks hold only
//! pointers, `int`, `uInt` and `uLong`, and `z_off_t` is `long`, the same on bionic and glibc
//! LP64. So every export binds straight to the host's `libz.so.1`, at no added cost.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use uxdroid_bridge::table::{Export, Table};

fn libz() -> usize {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    // SAFETY: a C string; a missing library leaves the handle null, and every lookup then fails.
    *HANDLE.get_or_init(|| unsafe {
        libc::dlopen(c"libz.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) as usize
    })
}

/// Binds a zlib export to the host's zlib.
fn zlib_symbol(name: &CStr) -> Option<*const c_void> {
    let handle = libz();
    if handle == 0 {
        return None;
    }
    // SAFETY: a handle from dlopen and a C string.
    let address = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
    (!address.is_null()).then_some(address.cast_const())
}

macro_rules! zlib {
    ($($name:literal: $signature:literal,)*) => {
        pub static TABLE: Table = Table {
            library: "libz.so",
            exports: &[$(Export::resolved($name, $signature, zlib_symbol),)*],
        };
    };
}

// The exports of the NDK's libz.so (zlib 1.3); `uLong` is u64, `uInt` u32, `z_off_t` i64.
zlib! {
    c"adler32": "u64(u64,ptr,u32)",
    c"adler32_combine": "u64(u64,u64,i64)",
    c"adler32_combine64": "u64(u64,u64,i64)",
    c"adler32_z": "u64(u64,ptr,u64)",
    c"compress": "i32(ptr,ptr,ptr,u64)",
    c"compress2": "i32(ptr,ptr,ptr,u64,i32)",
    c"compressBound": "u64(u64)",
    c"crc32": "u64(u64,ptr,u32)",
    c"crc32_combine": "u64(u64,u64,i64)",
    c"crc32_combine64": "u64(u64,u64,i64)",
    c"crc32_combine_gen": "u64(i64)",
    c"crc32_combine_gen64": "u64(i64)",
    c"crc32_combine_op": "u64(u64,u64,u64)",
    c"crc32_z": "u64(u64,ptr,u64)",
    c"deflate": "i32(ptr,i32)",
    c"deflateBound": "u64(ptr,u64)",
    c"deflateCopy": "i32(ptr,ptr)",
    c"deflateEnd": "i32(ptr)",
    c"deflateGetDictionary": "i32(ptr,ptr,ptr)",
    c"deflateInit2_": "i32(ptr,i32,i32,i32,i32,i32,ptr,i32)",
    c"deflateInit_": "i32(ptr,i32,ptr,i32)",
    c"deflateParams": "i32(ptr,i32,i32)",
    c"deflatePending": "i32(ptr,ptr,ptr)",
    c"deflatePrime": "i32(ptr,i32,i32)",
    c"deflateReset": "i32(ptr)",
    c"deflateResetKeep": "i32(ptr)",
    c"deflateSetDictionary": "i32(ptr,ptr,u32)",
    c"deflateSetHeader": "i32(ptr,ptr)",
    c"deflateTune": "i32(ptr,i32,i32,i32,i32)",
    c"get_crc_table": "ptr()",
    c"gzbuffer": "i32(ptr,u32)",
    c"gzclearerr": "void(ptr)",
    c"gzclose": "i32(ptr)",
    c"gzclose_r": "i32(ptr)",
    c"gzclose_w": "i32(ptr)",
    c"gzdirect": "i32(ptr)",
    c"gzdopen": "ptr(i32,ptr)",
    c"gzeof": "i32(ptr)",
    c"gzerror": "ptr(ptr,ptr)",
    c"gzflush": "i32(ptr,i32)",
    c"gzfread": "u64(ptr,u64,u64,ptr)",
    c"gzfwrite": "u64(ptr,u64,u64,ptr)",
    c"gzgetc": "i32(ptr)",
    c"gzgetc_": "i32(ptr)",
    c"gzgets": "ptr(ptr,ptr,i32)",
    c"gzoffset": "i64(ptr)",
    c"gzoffset64": "i64(ptr)",
    c"gzopen": "ptr(ptr,ptr)",
    c"gzopen64": "ptr(ptr,ptr)",
    c"gzprintf": "i32(ptr,ptr,...)",
    c"gzputc": "i32(ptr,i32)",
    c"gzputs": "i32(ptr,ptr)",
    c"gzread": "i32(ptr,ptr,u32)",
    c"gzrewind": "i32(ptr)",
    c"gzseek": "i64(ptr,i64,i32)",
    c"gzseek64": "i64(ptr,i64,i32)",
    c"gzsetparams": "i32(ptr,i32,i32)",
    c"gztell": "i64(ptr)",
    c"gztell64": "i64(ptr)",
    c"gzungetc": "i32(i32,ptr)",
    c"gzvprintf": "i32(ptr,ptr,va_list)",
    c"gzwrite": "i32(ptr,ptr,u32)",
    c"inflate": "i32(ptr,i32)",
    c"inflateBack": "i32(ptr,ptr,ptr,ptr,ptr)",
    c"inflateBackEnd": "i32(ptr)",
    c"inflateBackInit_": "i32(ptr,i32,ptr,ptr,i32)",
    c"inflateCodesUsed": "u64(ptr)",
    c"inflateCopy": "i32(ptr,ptr)",
    c"inflateEnd": "i32(ptr)",
    c"inflateGetDictionary": "i32(ptr,ptr,ptr)",
    c"inflateGetHeader": "i32(ptr,ptr)",
    c"inflateInit2_": "i32(ptr,i32,ptr,i32)",
    c"inflateInit_": "i32(ptr,ptr,i32)",
    c"inflateMark": "i64(ptr)",
    c"inflatePrime": "i32(ptr,i32,i32)",
    c"inflateReset": "i32(ptr)",
    c"inflateReset2": "i32(ptr,i32)",
    c"inflateResetKeep": "i32(ptr)",
    c"inflateSetDictionary": "i32(ptr,ptr,u32)",
    c"inflateSync": "i32(ptr)",
    c"inflateSyncPoint": "i32(ptr)",
    c"inflateUndermine": "i32(ptr,i32)",
    c"inflateValidate": "i32(ptr,i32)",
    c"uncompress": "i32(ptr,ptr,ptr,u64)",
    c"uncompress2": "i32(ptr,ptr,ptr,ptr)",
    c"zError": "ptr(i32)",
    c"zlibCompileFlags": "u64()",
    c"zlibVersion": "ptr()",
}
