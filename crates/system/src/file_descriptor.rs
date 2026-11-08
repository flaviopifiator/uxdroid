//! `java.io.FileDescriptor` for AOSP's host library.
//!
//! The x86_64 `libandroid_runtime.so` comes from Robolectric's android-all jar, built for OpenJDK, and
//! its copy of libnativehelper's `JniConstants` caches OpenJDK's `FileDescriptor.fd`. libcore names
//! that field `descriptor` (AOSP's own `libnativehelper/JniConstants.c`, which the arm64 library is
//! built from, says so too), so the first native that read a descriptor's int, such as
//! `ImageDecoder.nCreate` for a file or `BitmapFactory.nativeDecodeFileDescriptor`, aborted the
//! process with `Field not found: fd:I`.
//!
//! `JniConstants` looks its classes, methods and fields up once, under `pthread_once`, through the
//! function table of the first caller's `JNIEnv`. [`prime`] makes that first call itself, with a copy
//! of the table whose `GetFieldID` answers a missing `FileDescriptor.fd` with libcore's `descriptor`.
//! The library keeps that id, so every later read and write of a descriptor's int, on any thread,
//! reaches libcore's field, and no call after startup pays anything. The table is this thread's only
//! for the length of that one call.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use jni_sys::{JNI_TRUE, JNIEnv, JNINativeInterface_, jclass, jfieldID, jvalue};

type GetFieldId =
    unsafe extern "system" fn(*mut JNIEnv, jclass, *const c_char, *const c_char) -> jfieldID;

/// The runtime's own `GetFieldID`, which the copied table's answers.
static GET_FIELD_ID: AtomicUsize = AtomicUsize::new(0);

/// # Safety
/// Called through a JNI function table by code holding `env`, with valid strings.
unsafe extern "system" fn get_field_id(
    env: *mut JNIEnv,
    class: jclass,
    name: *const c_char,
    signature: *const c_char,
) -> jfieldID {
    // SAFETY: `prime` stores the runtime's GetFieldID before any table can call this.
    let original: GetFieldId =
        unsafe { std::mem::transmute::<usize, GetFieldId>(GET_FIELD_ID.load(Ordering::Acquire)) };
    // SAFETY: as above, the arguments are the caller's.
    let field = unsafe { original(env, class, name, signature) };
    // SAFETY: as above.
    if field.is_null() && unsafe { is_openjdk_descriptor(env, class, name, signature) } {
        // SAFETY: as above; the pending NoSuchFieldError was cleared.
        return unsafe { original(env, class, c"descriptor".as_ptr(), c"I".as_ptr()) };
    }
    field
}

/// Whether a failed lookup was OpenJDK's `int fd` of `java.io.FileDescriptor`. If so its
/// `NoSuchFieldError` is cleared; otherwise it stays pending, as the runtime raised it.
///
/// # Safety
/// As `get_field_id`, right after the runtime's lookup failed.
unsafe fn is_openjdk_descriptor(
    env: *mut JNIEnv,
    class: jclass,
    name: *const c_char,
    signature: *const c_char,
) -> bool {
    // SAFETY: as above.
    unsafe {
        if name.is_null() || signature.is_null() {
            return false;
        }
        if CStr::from_ptr(name) != c"fd" || CStr::from_ptr(signature) != c"I" {
            return false;
        }
        let jni = &**env;
        let pending = (jni.v1_1.ExceptionOccurred)(env);
        (jni.v1_1.ExceptionClear)(env);
        let descriptor = (jni.v1_1.FindClass)(env, c"java/io/FileDescriptor".as_ptr());
        (jni.v1_1.ExceptionClear)(env);
        let same =
            !descriptor.is_null() && (jni.v1_1.IsSameObject)(env, class, descriptor) == JNI_TRUE;
        if !descriptor.is_null() {
            (jni.v1_1.DeleteLocalRef)(env, descriptor);
        }
        if !pending.is_null() {
            if !same {
                (jni.v1_1.Throw)(env, pending);
            }
            (jni.v1_1.DeleteLocalRef)(env, pending);
        }
        same
    }
}

/// Makes the host library's first `JniConstants` lookup, through `BitmapFactory.nativeIsSeekable` on
/// a descriptor that names nothing (-1), whose only effect is a failed `lseek`. Must run before any
/// other native of that library reads a descriptor. A library whose `JniConstants` names libcore's
/// field, as the arm64 one built from AOSP's sources does, never reaches the substitution.
///
/// # Safety
/// `env` must be the calling thread's JNI environment, with no exception pending, after the host
/// library registered its natives.
pub unsafe fn prime(env: *mut JNIEnv) -> Result<(), String> {
    // SAFETY: as above. Lookups go through the runtime's table; the copy is installed only around the
    // one call and removed before anything else runs on this thread.
    unsafe {
        let runtime_table = *env;
        let jni = &*runtime_table;
        let base = runtime_table as usize;
        let slot = |entry: *const c_void| (entry as usize - base) / size_of::<usize>();
        let get_field_slot = slot(&raw const jni.v1_1.GetFieldID as *const c_void);
        // JNI 1.6's table, whose last entry is GetObjectRefType, is all the host library uses.
        let entries = slot(&raw const jni.v1_6.GetObjectRefType as *const c_void) + 1;

        let class = (jni.v1_1.FindClass)(env, c"android/graphics/BitmapFactory".as_ptr());
        if class.is_null() {
            (jni.v1_1.ExceptionClear)(env);
            return Err("android.graphics.BitmapFactory is not in the framework".into());
        }
        let method = (jni.v1_1.GetStaticMethodID)(
            env,
            class,
            c"nativeIsSeekable".as_ptr(),
            c"(Ljava/io/FileDescriptor;)Z".as_ptr(),
        );
        let descriptor_class = (jni.v1_1.FindClass)(env, c"java/io/FileDescriptor".as_ptr());
        let constructor = if descriptor_class.is_null() {
            std::ptr::null_mut()
        } else {
            (jni.v1_1.GetMethodID)(env, descriptor_class, c"<init>".as_ptr(), c"()V".as_ptr())
        };
        if method.is_null() || constructor.is_null() {
            (jni.v1_1.ExceptionClear)(env);
            return Err(
                "BitmapFactory.nativeIsSeekable(FileDescriptor) or FileDescriptor() is missing"
                    .into(),
            );
        }
        let descriptor =
            (jni.v1_1.NewObjectA)(env, descriptor_class, constructor, std::ptr::null());
        if descriptor.is_null() {
            (jni.v1_1.ExceptionClear)(env);
            return Err("cannot make a java.io.FileDescriptor".into());
        }

        let mut table = vec![0usize; size_of::<JNINativeInterface_>().div_ceil(size_of::<usize>())];
        std::ptr::copy_nonoverlapping(runtime_table as *const usize, table.as_mut_ptr(), entries);
        GET_FIELD_ID.store(jni.v1_1.GetFieldID as usize, Ordering::Release);
        table[get_field_slot] = get_field_id as *const () as usize;

        let argument = [jvalue { l: descriptor }];
        *env = table.as_ptr() as *const JNINativeInterface_;
        (jni.v1_1.CallStaticBooleanMethodA)(env, class, method, argument.as_ptr());
        *env = runtime_table;
        drop(table);

        let failed = (jni.v1_2.ExceptionCheck)(env) == JNI_TRUE;
        if failed {
            (jni.v1_1.ExceptionDescribe)(env);
        }
        for local in [descriptor, descriptor_class, class] {
            (jni.v1_1.DeleteLocalRef)(env, local);
        }
        if failed {
            return Err("BitmapFactory.nativeIsSeekable threw while priming JniConstants".into());
        }
        Ok(())
    }
}
