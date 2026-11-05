//! The natives of AOSP's `KeyCharacterMap`, over the seat's map in [`super::charmap`].
//!
//! Every native of the class is registered here, replacing the host library's, because the handle
//! is ours: a host native would read it as a pointer. Those with no meaning without a `.kcm` file
//! or a Parcel, `nativeApplyOverlay`, `nativeReadFromParcel` and `nativeWriteToParcel`, throw
//! `UnsupportedOperationException` naming themselves.

use jni_sys::{
    JNIEnv, jboolean, jcharArray, jclass, jint, jlong, jobject, jobjectArray, jstring, jvalue,
};

use super::charmap::{self, FULL};
use super::{SOURCE_KEYBOARD, keys};
use crate::handles::Handles;
use crate::java::{self, Field, Pending};
use crate::{Method, register, throw};

/// Maps by handle: each knows only the device it was made for.
static MAPS: Handles<jint> = Handles::new();

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn unsupported(env: *mut JNIEnv, method: &str) {
    // SAFETY: as above.
    unsafe {
        throw(
            env,
            c"java/lang/UnsupportedOperationException",
            &format!("uxdroid: unimplemented native android.view.KeyCharacterMap.{method}"),
        )
    };
}

unsafe extern "system" fn obtain_empty(env: *mut JNIEnv, _class: jclass, device: jint) -> jobject {
    let handle = MAPS.add(device);
    // SAFETY: ART passes a valid environment; `KeyCharacterMap(long)` adopts the handle.
    let made = unsafe {
        java::find_class(env, c"android/view/KeyCharacterMap")
            .and_then(|class| java::new_object(env, class, c"(J)V", &[jvalue { j: handle }]))
    };
    made.unwrap_or_else(|Pending| {
        MAPS.remove(handle);
        std::ptr::null_mut()
    })
}

unsafe extern "system" fn dispose(_env: *mut JNIEnv, _class: jclass, map: jlong) {
    MAPS.remove(map);
}

unsafe extern "system" fn character(
    _env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    key_code: jint,
    meta: jint,
) -> u16 {
    if MAPS.get(map).is_none() {
        return 0;
    }
    charmap::seat().character(key_code, meta)
}

unsafe extern "system" fn number(
    _env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    key_code: jint,
) -> u16 {
    if MAPS.get(map).is_none() {
        return 0;
    }
    charmap::seat().number(key_code)
}

unsafe extern "system" fn display_label(
    _env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    key_code: jint,
) -> u16 {
    if MAPS.get(map).is_none() {
        return 0;
    }
    charmap::seat().display_label(key_code)
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `chars` a char array or null.
unsafe fn chars_of(env: *mut JNIEnv, chars: jcharArray) -> Vec<u16> {
    if chars.is_null() {
        return Vec::new();
    }
    // SAFETY: as above; the vector holds the array's length.
    unsafe {
        let length = ((**env).v1_1.GetArrayLength)(env, chars);
        let mut out = vec![0u16; usize::try_from(length).unwrap_or(0)];
        ((**env).v1_1.GetCharArrayRegion)(env, chars, 0, length, out.as_mut_ptr());
        out
    }
}

unsafe extern "system" fn matching(
    env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    key_code: jint,
    chars: jcharArray,
    meta: jint,
) -> u16 {
    if MAPS.get(map).is_none() {
        return 0;
    }
    // SAFETY: ART passes a valid environment and a char array.
    let chars = unsafe { chars_of(env, chars) };
    charmap::seat().matching(key_code, &chars, meta)
}

unsafe extern "system" fn keyboard_type(_env: *mut JNIEnv, _class: jclass, _map: jlong) -> jint {
    FULL
}

unsafe extern "system" fn fallback(
    env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    key_code: jint,
    meta: jint,
    action: jobject,
) -> jboolean {
    let Some(fallback) = MAPS.get(map).and_then(|_| keys::fallback(key_code)) else {
        return false;
    };
    if action.is_null() {
        return false;
    }
    // SAFETY: ART passes a valid environment and a FallbackAction.
    unsafe {
        java::set_field(env, action, c"keyCode", Field::Int(fallback)).is_ok()
            && java::set_field(env, action, c"metaState", Field::Int(meta)).is_ok()
    }
}

unsafe extern "system" fn events(
    env: *mut JNIEnv,
    _class: jclass,
    map: jlong,
    chars: jcharArray,
) -> jobjectArray {
    let Some(device) = MAPS.get(map) else {
        return std::ptr::null_mut();
    };
    // SAFETY: ART passes a valid environment and a char array.
    let chars = unsafe { chars_of(env, chars) };
    let Some(presses) = charmap::seat().events(&chars) else {
        return std::ptr::null_mut();
    };
    let now = super::now() / 1_000_000;
    // SAFETY: as above; each element is set before the array is returned.
    unsafe {
        let Ok(key_class) = java::find_class(env, c"android/view/KeyEvent") else {
            return std::ptr::null_mut();
        };
        let array = ((**env).v1_1.NewObjectArray)(
            env,
            presses.len() as jint,
            key_class,
            std::ptr::null_mut(),
        );
        if array.is_null() {
            return array;
        }
        for (index, (action, key_code, meta)) in presses.into_iter().enumerate() {
            let args = [
                jvalue { j: now },
                jvalue { j: now },
                jvalue { i: action },
                jvalue { i: key_code },
                jvalue { i: 0 },
                jvalue { i: meta },
                jvalue { i: device },
                jvalue { i: 0 },
                jvalue { i: 0 },
                jvalue { i: SOURCE_KEYBOARD },
            ];
            let Ok(event) = java::new_object(env, key_class, c"(JJIIIIIIII)V", &args) else {
                return std::ptr::null_mut();
            };
            ((**env).v1_1.SetObjectArrayElement)(env, array, index as jint, event);
            java::delete_local(env, event);
        }
        array
    }
}

/// Every map reads the seat's, so two live maps are equal.
unsafe extern "system" fn equals(
    _env: *mut JNIEnv,
    _class: jclass,
    first: jlong,
    second: jlong,
) -> jboolean {
    MAPS.get(first).is_some() && MAPS.get(second).is_some()
}

/// The seat's keymap remaps no key.
unsafe extern "system" fn mapped_key(
    _env: *mut JNIEnv,
    _class: jclass,
    _map: jlong,
    key_code: jint,
) -> jint {
    key_code
}

unsafe extern "system" fn apply_overlay(
    env: *mut JNIEnv,
    _class: jclass,
    _map: jlong,
    _name: jstring,
    _overlay: jstring,
) {
    // SAFETY: ART passes a valid environment.
    unsafe { unsupported(env, "nativeApplyOverlay") };
}

unsafe extern "system" fn read_from_parcel(
    env: *mut JNIEnv,
    _class: jclass,
    _parcel: jobject,
) -> jlong {
    // SAFETY: ART passes a valid environment.
    unsafe { unsupported(env, "nativeReadFromParcel") };
    0
}

unsafe extern "system" fn write_to_parcel(
    env: *mut JNIEnv,
    _class: jclass,
    _map: jlong,
    _parcel: jobject,
) {
    // SAFETY: ART passes a valid environment.
    unsafe { unsupported(env, "nativeWriteToParcel") };
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 14] = [
        (
            c"nativeReadFromParcel",
            c"(Landroid/os/Parcel;)J",
            f!(read_from_parcel),
        ),
        (
            c"nativeWriteToParcel",
            c"(JLandroid/os/Parcel;)V",
            f!(write_to_parcel),
        ),
        (c"nativeDispose", c"(J)V", f!(dispose)),
        (c"nativeGetCharacter", c"(JII)C", f!(character)),
        (
            c"nativeGetFallbackAction",
            c"(JIILandroid/view/KeyCharacterMap$FallbackAction;)Z",
            f!(fallback),
        ),
        (c"nativeGetNumber", c"(JI)C", f!(number)),
        (c"nativeGetMatch", c"(JI[CI)C", f!(matching)),
        (c"nativeGetDisplayLabel", c"(JI)C", f!(display_label)),
        (c"nativeGetKeyboardType", c"(J)I", f!(keyboard_type)),
        (
            c"nativeGetEvents",
            c"(J[C)[Landroid/view/KeyEvent;",
            f!(events),
        ),
        (
            c"nativeObtainEmptyKeyCharacterMap",
            c"(I)Landroid/view/KeyCharacterMap;",
            f!(obtain_empty),
        ),
        (c"nativeEquals", c"(JJ)Z", f!(equals)),
        (
            c"nativeApplyOverlay",
            c"(JLjava/lang/String;Ljava/lang/String;)V",
            f!(apply_overlay),
        ),
        (c"nativeGetMappedKey", c"(JI)I", f!(mapped_key)),
    ];
    // SAFETY: the table matches the framework's declarations in Android 16.
    unsafe { register(env, c"android/view/KeyCharacterMap", &methods) }
}
