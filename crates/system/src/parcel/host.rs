//! Natives of AOSP's host library that take a `Parcel`.
//!
//! That library reads a parcel through `parcelForJavaObject`, whose field ids are set only by the
//! Parcel registration it never runs, so each of these calls aborted the process with
//! `fid == null`, and would read uxdroid's parcel as a C++ one if it got that far. Bitmap's pair is
//! replaced by `dev.uxdroid.system.BitmapParcels`, in Java over Bitmap's other natives. The rest
//! need the library's own C++ objects and throw `UnsupportedOperationException` naming the method.

use jni_sys::{JNIEnv, jboolean, jclass, jint, jlong, jobject, jvalue};

use crate::{Method, java, register, throw};

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn helper(env: *mut JNIEnv) -> Option<jclass> {
    // SAFETY: as above; uxdroid's Java is on the framework's class path.
    unsafe { java::find_class(env, c"dev/uxdroid/system/BitmapParcels").ok() }
}

unsafe extern "system" fn bitmap_write(
    env: *mut JNIEnv,
    _class: jclass,
    bitmap: jlong,
    density: jint,
    parcel: jobject,
) -> jboolean {
    // SAFETY: ART passes a valid environment; the arguments match `write(JILandroid/os/Parcel;)Z`.
    unsafe {
        let Some(class) = helper(env) else {
            return false;
        };
        let jni = &**env;
        let id = (jni.v1_1.GetStaticMethodID)(
            env,
            class,
            c"write".as_ptr(),
            c"(JILandroid/os/Parcel;)Z".as_ptr(),
        );
        if id.is_null() {
            return false;
        }
        let arguments = [
            jvalue { j: bitmap },
            jvalue { i: density },
            jvalue { l: parcel },
        ];
        (jni.v1_1.CallStaticBooleanMethodA)(env, class, id, arguments.as_ptr())
    }
}

unsafe extern "system" fn bitmap_read(
    env: *mut JNIEnv,
    _class: jclass,
    parcel: jobject,
) -> jobject {
    // SAFETY: as above, for `read(Landroid/os/Parcel;)Landroid/graphics/Bitmap;`.
    unsafe {
        let Some(class) = helper(env) else {
            return std::ptr::null_mut();
        };
        java::call_static_object(
            env,
            class,
            c"read",
            c"(Landroid/os/Parcel;)Landroid/graphics/Bitmap;",
            &[jvalue { l: parcel }],
        )
        .unwrap_or(std::ptr::null_mut())
    }
}

/// A native that refuses by name. Its arguments are declared, so each matches its Java signature.
macro_rules! refuse {
    ($function:ident, $name:literal, ($($argument:ident: $kind:ty),*) -> $result:ty) => {
        unsafe extern "system" fn $function(env: *mut JNIEnv, _class: jclass, $($argument: $kind),*) -> $result {
            $(let _ = $argument;)*
            let message = concat!($name, ": AOSP's host library cannot use uxdroid's parcels");
            // SAFETY: ART passes a valid environment.
            unsafe { throw(env, c"java/lang/UnsupportedOperationException", message) };
            Default::default()
        }
    };
}

refuse!(motion_read, "android.view.MotionEvent.nativeReadFromParcel", (event: jlong, parcel: jobject) -> jlong);
refuse!(motion_write, "android.view.MotionEvent.nativeWriteToParcel", (event: jlong, parcel: jobject) -> ());
refuse!(keys_read, "android.view.KeyCharacterMap.nativeReadFromParcel", (parcel: jobject) -> jlong);
refuse!(keys_write, "android.view.KeyCharacterMap.nativeWriteToParcel", (map: jlong, parcel: jobject) -> ());
refuse!(region_read, "android.graphics.Region.nativeCreateFromParcel", (parcel: jobject) -> jlong);
refuse!(region_write, "android.graphics.Region.nativeWriteToParcel", (region: jlong, parcel: jobject) -> jboolean);
refuse!(gainmap_read, "android.graphics.Gainmap.nReadGainmapFromParcel", (gainmap: jlong, parcel: jobject) -> ());
refuse!(gainmap_write, "android.graphics.Gainmap.nWriteGainmapToParcel", (gainmap: jlong, parcel: jobject) -> ());
refuse!(cursor_read, "android.database.CursorWindow.nativeCreateFromParcel", (parcel: jobject) -> jlong);
refuse!(cursor_write, "android.database.CursorWindow.nativeWriteToParcel", (window: jlong, parcel: jobject) -> ());

/// # Safety
/// `env` must be the calling thread's JNI environment, after AOSP's host library registered its own.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let bitmap: [Method; 2] = [
        (
            c"nativeWriteToParcel",
            c"(JILandroid/os/Parcel;)Z",
            f!(bitmap_write),
        ),
        (
            c"nativeCreateFromParcel",
            c"(Landroid/os/Parcel;)Landroid/graphics/Bitmap;",
            f!(bitmap_read),
        ),
    ];
    let motion: [Method; 2] = [
        (
            c"nativeReadFromParcel",
            c"(JLandroid/os/Parcel;)J",
            f!(motion_read),
        ),
        (
            c"nativeWriteToParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(motion_write),
        ),
    ];
    let keys: [Method; 2] = [
        (
            c"nativeReadFromParcel",
            c"(Landroid/os/Parcel;)J",
            f!(keys_read),
        ),
        (
            c"nativeWriteToParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(keys_write),
        ),
    ];
    let region: [Method; 2] = [
        (
            c"nativeCreateFromParcel",
            c"(Landroid/os/Parcel;)J",
            f!(region_read),
        ),
        (
            c"nativeWriteToParcel",
            c"(JLandroid/os/Parcel;)Z",
            f!(region_write),
        ),
    ];
    let gainmap: [Method; 2] = [
        (
            c"nReadGainmapFromParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(gainmap_read),
        ),
        (
            c"nWriteGainmapToParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(gainmap_write),
        ),
    ];
    let cursor: [Method; 2] = [
        (
            c"nativeCreateFromParcel",
            c"(Landroid/os/Parcel;)J",
            f!(cursor_read),
        ),
        (
            c"nativeWriteToParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(cursor_write),
        ),
    ];
    // SAFETY: each function matches its declaration in Android 16.
    unsafe {
        register(env, c"android/graphics/Bitmap", &bitmap)?;
        register(env, c"android/view/MotionEvent", &motion)?;
        register(env, c"android/view/KeyCharacterMap", &keys)?;
        register(env, c"android/graphics/Region", &region)?;
        register(env, c"android/graphics/Gainmap", &gainmap)?;
        register(env, c"android/database/CursorWindow", &cursor)?;
    }
    Ok(())
}
