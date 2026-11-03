//! Calls from our natives back into Java, over raw JNI.
//!
//! Each helper returns `Err(Pending)` when a Java exception is pending, such as a missing method or
//! one the callee threw. The native then returns at once, and Java sees the exception.

use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use jni_sys::{
    JNI_OK, JNI_VERSION_1_6, JNIEnv, JavaVM, jclass, jint, jlong, jmethodID, jobject, jvalue,
};

/// A Java exception is pending on this thread.
#[derive(Debug)]
pub struct Pending;

static VM: AtomicPtr<JavaVM> = AtomicPtr::new(std::ptr::null_mut());

pub fn set_vm(vm: *mut JavaVM) {
    VM.store(vm, Ordering::Release);
}

/// This thread's environment, if it is a Java thread. Free functions run on the registry's threads.
pub fn attached_env() -> Option<*mut JNIEnv> {
    let vm = VM.load(Ordering::Acquire);
    if vm.is_null() {
        return None;
    }
    let mut env: *mut c_void = std::ptr::null_mut();
    // SAFETY: `vm` came from JNI_OnLoad and lives as long as the process.
    let status = unsafe { ((**vm).v1_2.GetEnv)(vm, &mut env, JNI_VERSION_1_6) };
    (status == JNI_OK && !env.is_null()).then_some(env as *mut JNIEnv)
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn check(env: *mut JNIEnv) -> Result<(), Pending> {
    // SAFETY: as above.
    if unsafe { ((**env).v1_2.ExceptionCheck)(env) } {
        Err(Pending)
    } else {
        Ok(())
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn find_class(env: *mut JNIEnv, name: &CStr) -> Result<jclass, Pending> {
    // SAFETY: as above.
    let class = unsafe { ((**env).v1_1.FindClass)(env, name.as_ptr()) };
    if class.is_null() {
        Err(Pending)
    } else {
        Ok(class)
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live non-null reference.
pub unsafe fn method(
    env: *mut JNIEnv,
    object: jobject,
    name: &CStr,
    signature: &CStr,
) -> Result<jmethodID, Pending> {
    // SAFETY: as above. JNI looks methods up regardless of Java access.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let id = (jni.v1_1.GetMethodID)(env, class, name.as_ptr(), signature.as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if id.is_null() { Err(Pending) } else { Ok(id) }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, `object` a live non-null reference, and
/// `arguments` must match `signature`, which returns void.
pub unsafe fn call_void(
    env: *mut JNIEnv,
    object: jobject,
    name: &CStr,
    signature: &CStr,
    arguments: &[jvalue],
) -> Result<(), Pending> {
    // SAFETY: as above.
    unsafe {
        let id = method(env, object, name, signature)?;
        ((**env).v1_1.CallVoidMethodA)(env, object, id, arguments.as_ptr());
        check(env)
    }
}

/// # Safety
/// As [`call_void`], for a method returning an object.
pub unsafe fn call_object(
    env: *mut JNIEnv,
    object: jobject,
    name: &CStr,
    signature: &CStr,
    arguments: &[jvalue],
) -> Result<jobject, Pending> {
    // SAFETY: as above.
    unsafe {
        let id = method(env, object, name, signature)?;
        let result = ((**env).v1_1.CallObjectMethodA)(env, object, id, arguments.as_ptr());
        check(env)?;
        Ok(result)
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `arguments` must match `signature`.
pub unsafe fn new_object(
    env: *mut JNIEnv,
    class: jclass,
    signature: &CStr,
    arguments: &[jvalue],
) -> Result<jobject, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let id = (jni.v1_1.GetMethodID)(env, class, c"<init>".as_ptr(), signature.as_ptr());
        if id.is_null() {
            return Err(Pending);
        }
        let object = (jni.v1_1.NewObjectA)(env, class, id, arguments.as_ptr());
        check(env)?;
        if object.is_null() {
            Err(Pending)
        } else {
            Ok(object)
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `arguments` must match `signature`.
pub unsafe fn call_static_object(
    env: *mut JNIEnv,
    class: jclass,
    name: &CStr,
    signature: &CStr,
    arguments: &[jvalue],
) -> Result<jobject, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let id = (jni.v1_1.GetStaticMethodID)(env, class, name.as_ptr(), signature.as_ptr());
        if id.is_null() {
            return Err(Pending);
        }
        let result = (jni.v1_1.CallStaticObjectMethodA)(env, class, id, arguments.as_ptr());
        check(env)?;
        Ok(result)
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn static_object_field(
    env: *mut JNIEnv,
    class: jclass,
    name: &CStr,
    signature: &CStr,
) -> Result<jobject, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let id = (jni.v1_1.GetStaticFieldID)(env, class, name.as_ptr(), signature.as_ptr());
        if id.is_null() {
            return Err(Pending);
        }
        Ok((jni.v1_1.GetStaticObjectField)(env, class, id))
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live non-null reference.
pub unsafe fn object_field(
    env: *mut JNIEnv,
    object: jobject,
    name: &CStr,
    signature: &CStr,
) -> Result<jobject, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let id = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), signature.as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if id.is_null() {
            return Err(Pending);
        }
        Ok((jni.v1_1.GetObjectField)(env, object, id))
    }
}

/// A primitive field value to store.
pub enum Field {
    Int(jint),
    Long(jlong),
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live non-null reference.
pub unsafe fn set_field(
    env: *mut JNIEnv,
    object: jobject,
    name: &CStr,
    value: Field,
) -> Result<(), Pending> {
    let signature = match value {
        Field::Int(_) => c"I",
        Field::Long(_) => c"J",
    };
    // SAFETY: as above, and the setter matches the signature.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let id = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), signature.as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if id.is_null() {
            return Err(Pending);
        }
        match value {
            Field::Int(value) => (jni.v1_1.SetIntField)(env, object, id, value),
            Field::Long(value) => (jni.v1_1.SetLongField)(env, object, id, value),
        }
    }
    Ok(())
}

/// Turns a local reference into a global one, which any thread may use until it is deleted.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `local` a local reference or null.
pub unsafe fn global(env: *mut JNIEnv, local: jobject) -> jobject {
    if local.is_null() {
        return local;
    }
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let global = (jni.v1_1.NewGlobalRef)(env, local);
        (jni.v1_1.DeleteLocalRef)(env, local);
        global
    }
}

/// A global reference to an object the caller keeps its own reference to, such as an argument.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live reference or null.
pub unsafe fn keep(env: *mut JNIEnv, object: jobject) -> jobject {
    if object.is_null() {
        return object;
    }
    // SAFETY: as above.
    unsafe { ((**env).v1_1.NewGlobalRef)(env, object) }
}

/// Deletes global references from whichever Java thread drops their owner. A thread that is not
/// attached to Java cannot, so their objects stay alive: a leak, never a crash.
///
/// # Safety
/// Each reference must be a global reference or null, deleted only here.
pub unsafe fn delete_globals(references: &[jobject]) {
    let Some(env) = attached_env() else {
        return;
    };
    for &reference in references {
        if !reference.is_null() {
            // SAFETY: as above, on an attached thread.
            unsafe { ((**env).v1_1.DeleteGlobalRef)(env, reference) };
        }
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn delete_local(env: *mut JNIEnv, local: jobject) {
    if !local.is_null() {
        // SAFETY: as above.
        unsafe { ((**env).v1_1.DeleteLocalRef)(env, local) };
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live non-null reference.
pub unsafe fn long_field(env: *mut JNIEnv, object: jobject, name: &CStr) -> Result<jlong, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let id = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), c"J".as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if id.is_null() {
            return Err(Pending);
        }
        Ok((jni.v1_1.GetLongField)(env, object, id))
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment, and `object` a live non-null reference.
pub unsafe fn int_field(env: *mut JNIEnv, object: jobject, name: &CStr) -> Result<jint, Pending> {
    // SAFETY: as above.
    unsafe {
        let jni = &**env;
        let class = (jni.v1_1.GetObjectClass)(env, object);
        let id = (jni.v1_1.GetFieldID)(env, class, name.as_ptr(), c"I".as_ptr());
        (jni.v1_1.DeleteLocalRef)(env, class);
        if id.is_null() {
            return Err(Pending);
        }
        Ok((jni.v1_1.GetIntField)(env, object, id))
    }
}
