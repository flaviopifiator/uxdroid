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
pub mod control;
pub mod default_library;
pub mod defaults;
mod gl;
mod handles;
pub mod identity;
pub mod input;
mod java;
mod natives;
pub mod parcel;
mod process;
pub mod properties;
mod scene;
pub mod shared_memory;
mod surface;
mod vsync;
mod wayland;
mod windows;

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

/// Registers `methods` on the class named `name` (with dots), loaded through the system class
/// loader without being initialized, so its static initializer does not run now: it may call one of
/// these natives, and runs when the app first uses the class.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and each function must match its signature.
unsafe fn register_uninitialized(
    env: *mut JNIEnv,
    name: &CStr,
    methods: &[Method],
) -> Result<(), String> {
    let table: Vec<JNINativeMethod> = methods
        .iter()
        .map(|(method, signature, function)| JNINativeMethod {
            name: method.as_ptr() as *mut _,
            signature: signature.as_ptr() as *mut _,
            fnPtr: *function,
        })
        .collect();
    let failed = |what: &str| format!("{what} {}", name.to_string_lossy());
    // SAFETY: as above; Class and ClassLoader are in the boot class path, with these methods.
    unsafe {
        let jni = &**env;
        let class_class = (jni.v1_1.FindClass)(env, c"java/lang/Class".as_ptr());
        let loader_class = (jni.v1_1.FindClass)(env, c"java/lang/ClassLoader".as_ptr());
        let for_name = (jni.v1_1.GetStaticMethodID)(
            env,
            class_class,
            c"forName".as_ptr(),
            c"(Ljava/lang/String;ZLjava/lang/ClassLoader;)Ljava/lang/Class;".as_ptr(),
        );
        let system_loader = (jni.v1_1.GetStaticMethodID)(
            env,
            loader_class,
            c"getSystemClassLoader".as_ptr(),
            c"()Ljava/lang/ClassLoader;".as_ptr(),
        );
        if for_name.is_null() || system_loader.is_null() {
            clear_exception(env);
            return Err(failed("cannot look up"));
        }
        let loader =
            (jni.v1_1.CallStaticObjectMethodA)(env, loader_class, system_loader, std::ptr::null());
        let arguments = [
            jni_sys::jvalue {
                l: (jni.v1_1.NewStringUTF)(env, name.as_ptr()),
            },
            jni_sys::jvalue { z: false },
            jni_sys::jvalue { l: loader },
        ];
        let class =
            (jni.v1_1.CallStaticObjectMethodA)(env, class_class, for_name, arguments.as_ptr());
        if class.is_null() || (jni.v1_2.ExceptionCheck)(env) {
            clear_exception(env);
            return Err(failed("class not found:"));
        }
        if (jni.v1_1.RegisterNatives)(env, class, table.as_ptr(), table.len() as jint) != 0 {
            clear_exception(env);
            return Err(failed("registering natives failed:"));
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
        uxdroid_bridge::report!(
            "uxdroid: no default bodies for {} classes: {}",
            names.len(),
            names.join(" ")
        );
    }
    bound
}

/// `static native int writeDefaults(String list, String library)`: writes the library of default
/// bodies for every listed native, made once at install time, and returns how many it holds.
unsafe extern "system" fn write_defaults(
    env: *mut JNIEnv,
    _class: jclass,
    list: jstring,
    library: jstring,
) -> jint {
    // SAFETY: ART passes a valid environment and strings or null.
    let (list, library) = unsafe { (java_string(env, list), java_string(env, library)) };
    let result = match (list, library) {
        (Some(list), Some(library)) => std::fs::read_to_string(&list)
            .map_err(|error| format!("{list}: {error}"))
            .and_then(|text| {
                let natives = defaults::parse(&text);
                std::fs::write(&library, default_library::build(&natives))
                    .map_err(|error| format!("{library}: {error}"))
                    .map(|()| natives.len() as jint)
            }),
        _ => Err("no list or library path".to_string()),
    };
    result.unwrap_or_else(|error| {
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalStateException", &error) };
        0
    })
}

/// `static native String defaultsProblem(String library)`: why a library from `writeDefaults` cannot
/// be loaded as code, or null. On a `noexec` file system glibc cannot map it, and the native bridge
/// would copy it into private memory before `attachDefaults` refused it.
unsafe extern "system" fn defaults_problem(
    env: *mut JNIEnv,
    _class: jclass,
    library: jstring,
) -> jstring {
    // SAFETY: ART passes a valid environment and a string or null.
    let library = unsafe { java_string(env, library) }.unwrap_or_default();
    let problem = std::ffi::CString::new(library.as_str())
        .map_err(|_| "its path holds a NUL".to_string())
        .and_then(|path| {
            // SAFETY: a zeroed statvfs is a valid output buffer, and the path is a C string.
            let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(path.as_ptr(), &mut info) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if info.f_flag & libc::ST_NOEXEC != 0 {
                return Err("its file system does not allow running code".to_string());
            }
            Ok(())
        });
    match problem {
        Ok(()) => std::ptr::null_mut(),
        // SAFETY: as above.
        Err(problem) => unsafe { new_string(env, &problem) },
    }
}

/// `static native int attachDefaults(String library, String list)`: connects a library from
/// `writeDefaults`, which `System.load` loaded, to this one, and returns how many bodies it holds.
unsafe extern "system" fn attach_defaults(
    env: *mut JNIEnv,
    _class: jclass,
    library: jstring,
    list: jstring,
) -> jint {
    // SAFETY: ART passes a valid environment and strings or null.
    let (library, list) = unsafe { (java_string(env, library), java_string(env, list)) };
    #[cfg(target_arch = "x86_64")]
    let result = match (library, list) {
        (Some(library), Some(list)) => default_library::attach(&library, &list),
        _ => Err("no library or list path".to_string()),
    };
    #[cfg(not(target_arch = "x86_64"))]
    let result: Result<usize, String> = {
        let _ = (library, list);
        Err("install-time default bodies are x86_64 code".to_string())
    };
    result.map(|count| count as jint).unwrap_or_else(|error| {
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalStateException", &error) };
        0
    })
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
    // SAFETY: as above, and every table matches its Java declarations.
    if let Err(error) = unsafe { natives::register_all(env).and_then(|()| gl::register(env)) } {
        // SAFETY: as above.
        unsafe { throw(env, c"java/lang/IllegalStateException", &error) };
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `package` a string or null.
unsafe extern "system" fn application_uid(
    env: *mut JNIEnv,
    _class: jclass,
    package: jstring,
) -> jint {
    // SAFETY: as above.
    let package = unsafe { java_string(env, package) }.unwrap_or_default();
    identity::application_uid(&package) as jint
}

/// # Safety
/// As `application_uid`.
unsafe extern "system" fn become_application(
    env: *mut JNIEnv,
    _class: jclass,
    package: jstring,
) -> jint {
    // SAFETY: as above.
    let package = unsafe { java_string(env, package) }.unwrap_or_default();
    identity::become_application(&package) as jint
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
        let methods: [Method; 7] = [
            (
                c"defaultsProblem",
                c"(Ljava/lang/String;)Ljava/lang/String;",
                defaults_problem as *mut c_void,
            ),
            (
                c"bindDefaults",
                c"(Ljava/lang/String;)I",
                bind_defaults as *mut c_void,
            ),
            (
                c"writeDefaults",
                c"(Ljava/lang/String;Ljava/lang/String;)I",
                write_defaults as *mut c_void,
            ),
            (
                c"attachDefaults",
                c"(Ljava/lang/String;Ljava/lang/String;)I",
                attach_defaults as *mut c_void,
            ),
            (
                c"bindSystem",
                c"(Ljava/lang/String;)V",
                bind_system as *mut c_void,
            ),
            (
                c"applicationUid",
                c"(Ljava/lang/String;)I",
                application_uid as *mut c_void,
            ),
            (
                c"becomeApplication",
                c"(Ljava/lang/String;)I",
                become_application as *mut c_void,
            ),
        ];
        match register(env as *mut JNIEnv, c"dev/uxdroid/system/Natives", &methods) {
            Ok(()) => JNI_VERSION_1_6,
            Err(error) => {
                uxdroid_bridge::report!("uxdroid: {error}");
                JNI_ERR
            }
        }
    }
}
