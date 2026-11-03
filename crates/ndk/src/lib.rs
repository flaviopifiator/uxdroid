//! The Android NDK system libraries that app native code links against, implemented on the host.
//!
//! Each library is a signature table from `uxdroid_bridge::table`, declared here next to its
//! functions; the native bridge serves a table to every app library that names it in `DT_NEEDED`.
//! Everything here is host code with one ABI: a translator for another app ABI wraps these same
//! functions from the same tables, so nothing in this crate knows the app's architecture.
//!
//! `liblog` lives in the bridge itself, next to the printf machinery it shares with bionic's libc.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

mod aaudio;
mod assets;
mod bitmap;
mod choreographer;
mod egl;
mod egl_table;
mod gles_table;
mod java;
mod live;
mod looper;
mod opensles;
mod pixels;
mod window;

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
pub fn tables() -> [&'static Table; 7] {
    [
        &LIBANDROID,
        &LIBJNIGRAPHICS,
        &egl_table::EGL,
        &gles_table::GLESV2,
        &gles_table::GLESV3,
        &opensles::TABLE,
        &aaudio::TABLE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_is_valid() {
        for table in tables() {
            uxdroid_bridge::table::validate(table).unwrap();
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
        let opensles = tables()[5];
        assert!(opensles.find(b"SL_IID_ENGINE").unwrap().is_data());
    }
}
