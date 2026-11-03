//! What Android's system provides below its framework, for AOSP's framework on a Linux host.
//!
//! Startup order, driven by `dev.uxdroid.system.Boot`: this library loads, `bindDefaults` gives every
//! framework native a default body, AOSP's `libandroid_runtime.so` loads and replaces the ones it
//! implements, then `bindSystem` replaces the ones implemented here.
#![cfg(target_os = "linux")]

/// A native function as the `*mut c_void` that `RegisterNatives` takes.
macro_rules! f {
    ($function:expr) => {
        $function as *mut std::ffi::c_void
    };
}

mod audio;
mod compose;
pub mod defaults;
mod handles;
mod java;
mod natives;
pub mod parcel;
mod process;
pub mod properties;
pub mod shared_memory;
mod surface;
mod vsync;

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_void};

use jni_sys::{
    JNI_ERR, JNI_OK, JNI_VERSION_1_6, JNIEnv, JNINativeMethod, JavaVM, jclass, jint, jstring,
};

/// # Safety
/// `env` must be the calling thread's JNI environment, and `text` a valid string or null.
unsafe fn java_string(env: *mut JNIEnv, text: jstring) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: as above. The characters are copied out before being released.
    unsafe {
        let table = &**env;
        let chars = (table.v1_1.GetStringUTFChars)(env, text, std::ptr::null_mut());
        if chars.is_null() {
            return None;
        }
        let owned = CStr::from_ptr(chars).to_string_lossy().into_owned();
        (table.v1_1.ReleaseStringUTFChars)(env, text, chars);
        Some(owned)
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn new_string(env: *mut JNIEnv, text: &str) -> jstring {
    let text = CString::new(text.replace('\0', " ")).unwrap_or_default();
    // SAFETY: as above.
    unsafe { ((**env).v1_1.NewStringUTF)(env, text.as_ptr()) }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn throw(env: *mut JNIEnv, class: &CStr, message: &str) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: as above; both classes are in the boot class path.
    unsafe {
        let table = &**env;
        let class = (table.v1_1.FindClass)(env, class.as_ptr());
        (table.v1_1.ThrowNew)(env, class, text.as_ptr());
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
unsafe fn clear_exception(env: *mut JNIEnv) -> bool {
    // SAFETY: as above.
    unsafe {
        let table = &**env;
        let pending = (table.v1_2.ExceptionCheck)(env);
        if pending {
            (table.v1_1.ExceptionClear)(env);
        }
        pending
    }
}

/// A method to register: name, JNI signature, function.
type Method = (&'static CStr, &'static CStr, *mut c_void);

/// Registers `methods` on `class`, found through the caller's class loader.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and each function must match its signature.
unsafe fn register(env: *mut JNIEnv, class: &CStr, methods: &[Method]) -> Result<(), String> {
    let table: Vec<JNINativeMethod> = methods
        .iter()
        .map(|(name, signature, function)| JNINativeMethod {
            name: name.as_ptr() as *mut _,
            signature: signature.as_ptr() as *mut _,
            fnPtr: *function,
        })
        .collect();
    // SAFETY: as above; the names outlive the call.
    unsafe {
        let jni = &**env;
        let found = (jni.v1_1.FindClass)(env, class.as_ptr());
        if found.is_null() {
            clear_exception(env);
            return Err(format!("class {} not found", class.to_string_lossy()));
        }
        if (jni.v1_1.RegisterNatives)(env, found, table.as_ptr(), table.len() as jint) != 0 {
            clear_exception(env);
            return Err(format!(
                "registering natives of {} failed",
                class.to_string_lossy()
            ));
        }
    }
    Ok(())
}

/// `static native int bindDefaults(String list)`: binds a default body to every listed native and
/// returns how many were bound.
unsafe extern "system" fn bind_defaults(env: *mut JNIEnv, _class: jclass, path: jstring) -> jint {
    // SAFETY: ART passes a valid environment and string.
    let Some(path) = (unsafe { java_string(env, path) }) else {
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/NullPointerException", "no list of natives") };
        return 0;
    };
    let list = match std::fs::read_to_string(&path) {
        Ok(list) => list,
        Err(error) => {
            // SAFETY: as above.
            unsafe {
                throw(
                    env,
                    c"java/lang/IllegalStateException",
                    &format!("{path}: {error}"),
                )
            };
            return 0;
        }
    };
    let natives = defaults::parse(&list);
    #[cfg(target_arch = "x86_64")]
    let addresses = match defaults::stubs(&natives) {
        Ok(addresses) => addresses,
        Err(error) => {
            // SAFETY: as above.
            unsafe { throw(env, c"java/lang/IllegalStateException", &error) };
            return 0;
        }
    };
    #[cfg(not(target_arch = "x86_64"))]
    let addresses: Vec<usize> = Vec::new();

    let mut by_class: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, native) in natives.iter().enumerate() {
        by_class.entry(&native.class).or_default().push(index);
    }
    let mut bound = 0;
    let mut skipped = Vec::new();
    for (class, indices) in by_class {
        let names: Vec<CString> = indices
            .iter()
            .map(|&i| CString::new(natives[i].name.as_str()).unwrap_or_default())
            .collect();
        let signatures: Vec<CString> = indices
            .iter()
            .map(|&i| CString::new(natives[i].signature.as_str()).unwrap_or_default())
            .collect();
        let table: Vec<JNINativeMethod> = indices
            .iter()
            .zip(names.iter().zip(&signatures))
            .filter_map(|(&i, (name, signature))| {
                Some(JNINativeMethod {
                    name: name.as_ptr() as *mut _,
                    signature: signature.as_ptr() as *mut _,
                    fnPtr: *addresses.get(i)? as *mut c_void,
                })
            })
            .collect();
        let Ok(class) = CString::new(class) else {
            continue;
        };
        // SAFETY: each stub ignores its arguments and returns zero for any return type.
        unsafe {
            let jni = &**env;
            let found = (jni.v1_1.FindClass)(env, class.as_ptr());
            if found.is_null() {
                clear_exception(env);
                skipped.push(class);
                continue;
            }
            if (jni.v1_1.RegisterNatives)(env, found, table.as_ptr(), table.len() as jint) == 0 {
                bound += table.len() as jint;
            } else {
                clear_exception(env);
                skipped.push(class);
            }
        }
    }
    // A class that does not load cannot call its natives either, so this only matters when debugging.
    if !skipped.is_empty() && natives::min_priority() <= natives::DEBUG {
        let names: Vec<_> = skipped.iter().map(|c| c.to_string_lossy()).collect();
        eprintln!(
            "uxdroid: no default bodies for {} classes: {}",
            names.len(),
            names.join(" ")
        );
    }
    bound
}

/// `static native void bindSystem(String buildProp)`: loads properties, then replaces default bodies
/// with this library's natives.
unsafe extern "system" fn bind_system(env: *mut JNIEnv, _class: jclass, build_prop: jstring) {
    // SAFETY: ART passes a valid environment and string.
    if let Some(path) = unsafe { java_string(env, build_prop) } {
        match std::fs::read_to_string(&path) {
            Ok(text) => natives::PROPERTIES.load(&text),
            Err(error) => {
                // SAFETY: as above.
                unsafe {
                    throw(
                        env,
                        c"java/lang/IllegalStateException",
                        &format!("{path}: {error}"),
                    )
                };
                return;
            }
        }
    }
    // SAFETY: as above, and every table matches its Java declarations.
    if let Err(error) = unsafe { natives::register_all(env) } {
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalStateException", &error) };
    }
}

/// # Safety
/// Called by ART when the library loads.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _reserved: *mut c_void) -> jint {
    let _ = surface::LOADED.set(std::time::Instant::now());
    java::set_vm(vm);
    let mut env: *mut c_void = std::ptr::null_mut();
    // SAFETY: ART passes a valid VM, and this thread is attached.
    unsafe {
        if ((**vm).v1_2.GetEnv)(vm, &mut env, JNI_VERSION_1_6) != JNI_OK {
            return JNI_ERR;
        }
        let methods: [Method; 2] = [
            (
                c"bindDefaults",
                c"(Ljava/lang/String;)I",
                bind_defaults as *mut c_void,
            ),
            (
                c"bindSystem",
                c"(Ljava/lang/String;)V",
                bind_system as *mut c_void,
            ),
        ];
        match register(env as *mut JNIEnv, c"dev/uxdroid/system/Natives", &methods) {
            Ok(()) => JNI_VERSION_1_6,
            Err(error) => {
                eprintln!("uxdroid: {error}");
                JNI_ERR
            }
        }
    }
}
