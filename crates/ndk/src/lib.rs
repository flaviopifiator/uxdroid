//! The Android NDK system libraries that app native code links against, implemented on the host.
//!
//! Each library is a signature table from `uxdroid_bridge::table`, declared here next to its
//! functions; the native bridge serves a table to every app library that names it in `DT_NEEDED`.
//! Everything here is host code with one ABI: a translator for another app ABI wraps these same
//! functions from the same tables, so nothing in this crate knows the app's architecture.
//!
//! `liblog` lives in the bridge itself, next to the printf machinery it shares with bionic's libc.
#![cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux",
    target_env = "gnu"
))]

mod aaudio;
mod assets;
mod bitmap;
mod choreographer;
mod egl;
mod egl_table;
mod gles_table;
mod gpu;
mod hardware_buffer;
mod java;
mod live;
mod looper;
mod media;
mod opensles;
mod pixels;
pub mod present;
mod shared_memory;
mod window;
mod zlib;

/// The EGL, GLES and native window functions, for the framework's Java GL bindings in
/// `libuxdroid_system.so` (`android.opengl` and `com.google.android.gles_jni`), which are JNI
/// natives of host code, not app code, and so take these functions directly rather than through a
/// signature table. That library holds its own copy of this crate's state: a surface made through
/// the Java API is swapped through it, as an app that uses one API does.
pub mod host_gl {
    pub use crate::egl::{
        choose_config, create_window_surface, destroy_surface, egl_symbol, get_config_attrib,
        get_display, gles_symbol, swap_buffers,
    };
    pub use crate::window::{from_surface as window_from_surface, release as release_window};
}

use uxdroid_bridge::table::{Export, Table};

macro_rules! function {
    ($name:literal, $signature:literal, $function:path) => {
        Export::function($name, $signature, $function as *const ())
    };
}

pub static LIBANDROID: Table = Table {
    library: "libandroid.so",
    exports: &[
        function!(c"AAssetDir_close", "void(ptr)", assets::dir_close),
        function!(c"AAssetDir_getNextFileName", "ptr(ptr)", assets::dir_next),
        function!(c"AAssetDir_rewind", "void(ptr)", assets::dir_rewind),
        function!(
            c"AAssetManager_fromJava",
            "ptr(ptr,ptr)",
            assets::manager_from_java
        ),
        function!(
            c"AAssetManager_open",
            "ptr(ptr,ptr,i32)",
            assets::manager_open
        ),
        function!(
            c"AAssetManager_openDir",
            "ptr(ptr,ptr)",
            assets::manager_open_dir
        ),
        function!(c"AAsset_close", "void(ptr)", assets::asset_close),
        function!(c"AAsset_getBuffer", "ptr(ptr)", assets::asset_get_buffer),
        function!(c"AAsset_getLength", "i64(ptr)", assets::asset_get_length),
        function!(c"AAsset_getLength64", "i64(ptr)", assets::asset_get_length),
        function!(
            c"AAsset_getRemainingLength",
            "i64(ptr)",
            assets::asset_get_remaining
        ),
        function!(
            c"AAsset_getRemainingLength64",
            "i64(ptr)",
            assets::asset_get_remaining
        ),
        function!(
            c"AAsset_isAllocated",
            "i32(ptr)",
            assets::asset_is_allocated
        ),
        function!(
            c"AAsset_openFileDescriptor",
            "i32(ptr,ptr,ptr)",
            assets::asset_open_fd
        ),
        function!(
            c"AAsset_openFileDescriptor64",
            "i32(ptr,ptr,ptr)",
            assets::asset_open_fd
        ),
        function!(c"AAsset_read", "i32(ptr,ptr,u64)", assets::asset_read),
        function!(c"AAsset_seek", "i64(ptr,i64,i32)", assets::asset_seek),
        function!(c"AAsset_seek64", "i64(ptr,i64,i32)", assets::asset_seek),
        function!(
            c"AChoreographerFrameCallbackData_getFrameTimeNanos",
            "i64(ptr)",
            choreographer::data_frame_time
        ),
        function!(
            c"AChoreographerFrameCallbackData_getFrameTimelineDeadlineNanos",
            "i64(ptr,u64)",
            choreographer::data_deadline
        ),
        function!(
            c"AChoreographerFrameCallbackData_getFrameTimelineExpectedPresentationTimeNanos",
            "i64(ptr,u64)",
            choreographer::data_expected_presentation
        ),
        function!(
            c"AChoreographerFrameCallbackData_getFrameTimelineVsyncId",
            "i64(ptr,u64)",
            choreographer::data_vsync_id
        ),
        function!(
            c"AChoreographerFrameCallbackData_getFrameTimelinesLength",
            "u64(ptr)",
            choreographer::data_timelines_length
        ),
        function!(
            c"AChoreographerFrameCallbackData_getPreferredFrameTimelineIndex",
            "u64(ptr)",
            choreographer::data_preferred_timeline
        ),
        function!(
            c"AChoreographer_getInstance",
            "ptr()",
            choreographer::get_instance
        ),
        function!(
            c"AChoreographer_postFrameCallback",
            "void(ptr,ptr,ptr)",
            choreographer::post_frame_callback
        ),
        function!(
            c"AChoreographer_postFrameCallback64",
            "void(ptr,ptr,ptr)",
            choreographer::post_frame_callback64
        ),
        function!(
            c"AChoreographer_postFrameCallbackDelayed",
            "void(ptr,ptr,ptr,i64)",
            choreographer::post_frame_callback_delayed
        ),
        function!(
            c"AChoreographer_postFrameCallbackDelayed64",
            "void(ptr,ptr,ptr,u32)",
            choreographer::post_frame_callback_delayed64
        ),
        function!(
            c"AChoreographer_postVsyncCallback",
            "void(ptr,ptr,ptr)",
            choreographer::post_vsync_callback
        ),
        function!(
            c"AChoreographer_registerRefreshRateCallback",
            "void(ptr,ptr,ptr)",
            choreographer::register_refresh_rate_callback
        ),
        function!(
            c"AChoreographer_unregisterRefreshRateCallback",
            "void(ptr,ptr,ptr)",
            choreographer::unregister_refresh_rate_callback
        ),
        function!(
            c"AHardwareBuffer_acquire",
            "void(ptr)",
            hardware_buffer::acquire
        ),
        function!(
            c"AHardwareBuffer_allocate",
            "i32(ptr,ptr)",
            hardware_buffer::allocate
        ),
        function!(
            c"AHardwareBuffer_describe",
            "void(ptr,ptr)",
            hardware_buffer::describe
        ),
        function!(
            c"AHardwareBuffer_lock",
            "i32(ptr,u64,i32,ptr,ptr)",
            hardware_buffer::lock
        ),
        function!(
            c"AHardwareBuffer_release",
            "void(ptr)",
            hardware_buffer::release
        ),
        function!(
            c"AHardwareBuffer_unlock",
            "i32(ptr,ptr)",
            hardware_buffer::unlock
        ),
        function!(c"ALooper_acquire", "void(ptr)", looper::acquire),
        function!(
            c"ALooper_addFd",
            "i32(ptr,i32,i32,i32,ptr,ptr)",
            looper::add_fd
        ),
        function!(c"ALooper_forThread", "ptr()", looper::for_thread),
        function!(c"ALooper_pollAll", "i32(i32,ptr,ptr,ptr)", looper::poll_all),
        function!(
            c"ALooper_pollOnce",
            "i32(i32,ptr,ptr,ptr)",
            looper::poll_once
        ),
        function!(c"ALooper_prepare", "ptr(i32)", looper::prepare),
        function!(c"ALooper_release", "void(ptr)", looper::release),
        function!(c"ALooper_removeFd", "i32(ptr,i32)", looper::remove_fd),
        function!(c"ALooper_wake", "void(ptr)", looper::wake),
        function!(c"ANativeWindow_acquire", "void(ptr)", window::acquire),
        function!(
            c"ANativeWindow_fromSurface",
            "ptr(ptr,ptr)",
            window::from_surface
        ),
        function!(c"ANativeWindow_getFormat", "i32(ptr)", window::get_format),
        function!(c"ANativeWindow_getHeight", "i32(ptr)", window::get_height),
        function!(c"ANativeWindow_getWidth", "i32(ptr)", window::get_width),
        function!(c"ANativeWindow_lock", "i32(ptr,ptr,ptr)", window::lock),
        function!(c"ANativeWindow_release", "void(ptr)", window::release),
        function!(
            c"ANativeWindow_setBuffersGeometry",
            "i32(ptr,i32,i32,i32)",
            window::set_buffers_geometry
        ),
        function!(
            c"ANativeWindow_toSurface",
            "ptr(ptr,ptr)",
            window::to_surface
        ),
        function!(
            c"ANativeWindow_unlockAndPost",
            "i32(ptr)",
            window::unlock_and_post
        ),
        function!(
            c"ASharedMemory_create",
            "i32(ptr,u64)",
            shared_memory::create
        ),
        function!(
            c"ASharedMemory_getSize",
            "u64(i32)",
            shared_memory::get_size
        ),
        function!(
            c"ASharedMemory_setProt",
            "i32(i32,i32)",
            shared_memory::set_prot
        ),
    ],
};

pub static LIBJNIGRAPHICS: Table = Table {
    library: "libjnigraphics.so",
    exports: &[
        function!(
            c"AndroidBitmap_getInfo",
            "i32(ptr,ptr,ptr)",
            bitmap::get_info
        ),
        function!(
            c"AndroidBitmap_lockPixels",
            "i32(ptr,ptr,ptr)",
            bitmap::lock_pixels
        ),
        function!(
            c"AndroidBitmap_unlockPixels",
            "i32(ptr,ptr)",
            bitmap::unlock_pixels
        ),
    ],
};

/// The signatures of OpenSL ES's method tables and of the callbacks into app code, which reach app
/// code by pointer instead of by name; a translator wraps them from these.
pub use opensles::interfaces as opensles_interfaces;

/// Every table this crate serves.
pub fn tables() -> [&'static Table; 10] {
    [
        &LIBANDROID,
        &LIBJNIGRAPHICS,
        &egl_table::EGL,
        &gles_table::GLESV2,
        &gles_table::GLESV3,
        &gles_table::GLES_EXTENSIONS,
        &opensles::TABLE,
        &aaudio::TABLE,
        &media::TABLE,
        &zlib::TABLE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_memory_regions_have_their_size_and_map() {
        let function = |name: &[u8]| {
            LIBANDROID
                .find(name)
                .and_then(|export| export.bind())
                .unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(name)))
        };
        type Create = unsafe extern "C" fn(*const std::ffi::c_char, usize) -> i32;
        type GetSize = unsafe extern "C" fn(i32) -> usize;
        type SetProt = unsafe extern "C" fn(i32, i32) -> i32;
        // SAFETY: the NDK's signatures of these three functions.
        unsafe {
            let create: Create = std::mem::transmute(function(b"ASharedMemory_create"));
            let get_size: GetSize = std::mem::transmute(function(b"ASharedMemory_getSize"));
            let set_prot: SetProt = std::mem::transmute(function(b"ASharedMemory_setProt"));
            let fd = create(c"startup".as_ptr(), 8192);
            assert!(fd >= 0);
            assert_eq!(get_size(fd), 8192);
            let map = libc::mmap(
                std::ptr::null_mut(),
                8192,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            assert_ne!(map, libc::MAP_FAILED);
            map.cast::<u8>().write(7);
            libc::munmap(map, 8192);
            assert_eq!(set_prot(fd, libc::PROT_READ), 0);
            libc::close(fd);
            assert_eq!(create(c"empty".as_ptr(), 0), -1);
            assert_eq!(get_size(-1), 0);
        }
    }

    #[test]
    fn hardware_buffers_keep_their_pixels_and_description() {
        let bind = |name: &[u8]| {
            LIBANDROID
                .find(name)
                .and_then(|export| export.bind())
                .unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(name)))
        };
        type Allocate = unsafe extern "C" fn(*const [u32; 10], *mut *mut std::ffi::c_void) -> i32;
        type Describe = unsafe extern "C" fn(*const std::ffi::c_void, *mut [u32; 10]);
        type Lock = unsafe extern "C" fn(
            *mut std::ffi::c_void,
            u64,
            i32,
            *const std::ffi::c_void,
            *mut *mut u8,
        ) -> i32;
        type Unlock = unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32) -> i32;
        type Release = unsafe extern "C" fn(*mut std::ffi::c_void);
        const CPU_READ_OFTEN: u64 = 3;
        const CPU_WRITE_OFTEN: u64 = 3 << 4;
        // AHardwareBuffer_Desc: width, height, layers, format, usage (u64), stride, rfu0, rfu1 (u64).
        let usage = CPU_READ_OFTEN | CPU_WRITE_OFTEN;
        let desc: [u32; 10] = [16, 8, 1, 1, usage as u32, 0, 0, 0, 0, 0];
        // SAFETY: the NDK's signatures; the descriptions are AHardwareBuffer_Desc's 40 bytes.
        unsafe {
            let allocate: Allocate = std::mem::transmute(bind(b"AHardwareBuffer_allocate"));
            let describe: Describe = std::mem::transmute(bind(b"AHardwareBuffer_describe"));
            let lock: Lock = std::mem::transmute(bind(b"AHardwareBuffer_lock"));
            let unlock: Unlock = std::mem::transmute(bind(b"AHardwareBuffer_unlock"));
            let release: Release = std::mem::transmute(bind(b"AHardwareBuffer_release"));
            let mut buffer = std::ptr::null_mut();
            assert_eq!(allocate(&desc, &mut buffer), 0);
            let mut seen = [0u32; 10];
            describe(buffer, &mut seen);
            assert_eq!(&seen[..7], &[16, 8, 1, 1, usage as u32, 0, 16]);
            let mut pixels = std::ptr::null_mut();
            assert_eq!(
                lock(buffer, CPU_WRITE_OFTEN, -1, std::ptr::null(), &mut pixels),
                0
            );
            pixels.add(16 * 4 * 7 + 4 * 15).write(0xab);
            let mut fence = 0;
            assert_eq!(unlock(buffer, &mut fence), 0);
            assert_eq!(fence, -1);
            assert_eq!(
                lock(buffer, CPU_READ_OFTEN, -1, std::ptr::null(), &mut pixels),
                0
            );
            assert_eq!(pixels.add(16 * 4 * 7 + 4 * 15).read(), 0xab);
            unlock(buffer, std::ptr::null_mut());
            // A lock for the GPU alone gives the CPU nothing to touch.
            assert_ne!(lock(buffer, 1 << 8, -1, std::ptr::null(), &mut pixels), 0);
            release(buffer);
            let unknown: [u32; 10] = [16, 8, 1, 0x7777, usage as u32, 0, 0, 0, 0, 0];
            assert_ne!(allocate(&unknown, &mut buffer), 0);
        }
    }

    #[test]
    fn hardware_buffers_are_not_offered_to_egl_yet() {
        let address = egl_table::EGL
            .find(b"eglGetNativeClientBufferANDROID")
            .and_then(|export| export.bind())
            .expect("eglGetNativeClientBufferANDROID binds without Mesa having it");
        // SAFETY: the extension's signature; the buffer is never read.
        let client = unsafe {
            let get: unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void =
                std::mem::transmute(address);
            get(std::ptr::dangling())
        };
        assert!(client.is_null());
    }

    #[test]
    fn libz_binds_to_the_host_zlib() {
        let zlib = tables()
            .into_iter()
            .find(|table| table.library == "libz.so")
            .expect("a libz.so table");
        // What Instagram 454's x86_64 libraries import from libz.so.
        for name in [
            "adler32",
            "compress2",
            "compressBound",
            "crc32",
            "deflate",
            "deflateBound",
            "deflateEnd",
            "deflateInit2_",
            "deflateReset",
            "inflate",
            "inflateEnd",
            "inflateInit2_",
            "inflateInit_",
            "inflateReset",
            "inflateReset2",
            "uncompress",
        ] {
            let export = zlib
                .find(name.as_bytes())
                .unwrap_or_else(|| panic!("{name}"));
            assert!(export.bind().is_some(), "{name} binds to the host's zlib");
        }
        let version = zlib.find(b"zlibVersion").unwrap().bind().unwrap();
        // SAFETY: zlibVersion takes nothing and returns a static C string.
        let version = unsafe {
            let version: unsafe extern "C" fn() -> *const std::ffi::c_char =
                std::mem::transmute(version);
            std::ffi::CStr::from_ptr(version())
        };
        assert!(version.to_bytes().starts_with(b"1."), "{version:?}");
    }

    #[test]
    fn every_table_is_valid() {
        for table in tables() {
            uxdroid_bridge::table::validate(table).unwrap();
        }
    }

    #[test]
    fn extension_functions_skia_asks_for_are_signed() {
        // What RustDesk 1.5.1's arm64 Skia looks up through eglGetProcAddress, from Mesa's list.
        for name in [
            "glBindFragDataLocationEXT",
            "glBindFragDataLocationIndexedEXT",
            "glClearTexImageEXT",
            "glClearTexSubImageEXT",
            "glDrawArraysInstancedBaseInstanceEXT",
            "glDrawElementsInstancedBaseVertexBaseInstanceEXT",
            "glMultiDrawArraysIndirectEXT",
            "glMultiDrawElementsIndirectEXT",
            "glTextureBarrierNV",
        ] {
            assert!(
                tables()
                    .iter()
                    .any(|table| table.find(name.as_bytes()).is_some()),
                "{name} is not signed"
            );
        }
    }

    #[test]
    fn the_tables_cover_what_rustdesk_imports() {
        // Measured on RustDesk 1.5.1's x86_64 libflutter.so and librustdesk.so.
        let imports: &[(&str, &[&str])] = &[
            (
                "libandroid.so",
                &[
                    "AAssetManager_fromJava",
                    "AAssetManager_open",
                    "AAsset_close",
                    "AAsset_getBuffer",
                    "AAsset_getLength",
                    "AAsset_isAllocated",
                    "ALooper_acquire",
                    "ALooper_addFd",
                    "ALooper_forThread",
                    "ALooper_pollOnce",
                    "ALooper_prepare",
                    "ALooper_release",
                    "ALooper_removeFd",
                    "ALooper_wake",
                    "ANativeWindow_fromSurface",
                    "ANativeWindow_getFormat",
                    "ANativeWindow_getHeight",
                    "ANativeWindow_getWidth",
                    "ANativeWindow_lock",
                    "ANativeWindow_release",
                    "ANativeWindow_unlockAndPost",
                ],
            ),
            (
                "libjnigraphics.so",
                &[
                    "AndroidBitmap_getInfo",
                    "AndroidBitmap_lockPixels",
                    "AndroidBitmap_unlockPixels",
                ],
            ),
            (
                "libEGL.so",
                &[
                    "eglChooseConfig",
                    "eglCreateContext",
                    "eglCreateImageKHR",
                    "eglCreatePbufferSurface",
                    "eglCreateWindowSurface",
                    "eglDestroyContext",
                    "eglDestroyImageKHR",
                    "eglDestroySurface",
                    "eglGetCurrentContext",
                    "eglGetCurrentDisplay",
                    "eglGetCurrentSurface",
                    "eglGetDisplay",
                    "eglGetError",
                    "eglGetProcAddress",
                    "eglInitialize",
                    "eglMakeCurrent",
                    "eglQueryString",
                    "eglQuerySurface",
                    "eglSwapBuffers",
                    "eglTerminate",
                ],
            ),
            (
                "libOpenSLES.so",
                &[
                    "SL_IID_ANDROIDCONFIGURATION",
                    "SL_IID_ANDROIDSIMPLEBUFFERQUEUE",
                    "SL_IID_BUFFERQUEUE",
                    "SL_IID_ENGINE",
                    "SL_IID_PLAY",
                    "SL_IID_RECORD",
                ],
            ),
        ];
        for (library, names) in imports {
            let table = tables()
                .into_iter()
                .find(|t| t.library == *library)
                .unwrap();
            for name in *names {
                assert!(
                    table.find(name.as_bytes()).is_some(),
                    "{library} lacks {name}"
                );
            }
        }
        let opensles = tables()
            .into_iter()
            .find(|table| table.library == "libOpenSLES.so")
            .unwrap();
        assert!(opensles.find(b"SL_IID_ENGINE").unwrap().is_data());
    }

    #[test]
    fn the_tables_cover_what_arm64_rustdesk_imports_from_libmediandk() {
        // Measured with readelf on RustDesk 1.5.1's arm64-v8a librustdesk.so, which the x86_64
        // build does not link; a translated install refuses a library whose DT_NEEDED no table serves.
        let imports = [
            "AMediaCodec_configure",
            "AMediaCodec_createCodecByName",
            "AMediaCodec_createDecoderByType",
            "AMediaCodec_createEncoderByType",
            "AMediaCodec_delete",
            "AMediaCodec_dequeueInputBuffer",
            "AMediaCodec_dequeueOutputBuffer",
            "AMediaCodec_flush",
            "AMediaCodec_getInputBuffer",
            "AMediaCodec_getOutputBuffer",
            "AMediaCodec_getOutputFormat",
            "AMediaCodec_queueInputBuffer",
            "AMediaCodec_releaseOutputBuffer",
            "AMediaCodec_releaseOutputBufferAtTime",
            "AMediaCodec_start",
            "AMediaCodec_stop",
            "AMediaFormat_delete",
            "AMediaFormat_getBuffer",
            "AMediaFormat_getFloat",
            "AMediaFormat_getInt32",
            "AMediaFormat_getInt64",
            "AMediaFormat_getString",
            "AMediaFormat_new",
            "AMediaFormat_setBuffer",
            "AMediaFormat_setFloat",
            "AMediaFormat_setInt32",
            "AMediaFormat_setInt64",
            "AMediaFormat_setString",
            "AMediaFormat_toString",
        ];
        assert_eq!(imports.len(), 29);
        let table = tables()
            .into_iter()
            .find(|t| t.library == "libmediandk.so")
            .expect("no table serves libmediandk.so");
        for name in imports {
            let export = table.find(name.as_bytes());
            assert!(
                export.is_some_and(|e| !e.is_data()),
                "libmediandk.so lacks {name}"
            );
        }
    }
}
