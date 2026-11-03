//! JNI calls from NDK functions into the framework, on whichever thread app code calls from.
//!
//! NDK functions never leave a Java exception pending: app code calling them cannot expect one.
//! Each failed call clears the exception, prints it at most once per call site, and returns
//! [`Thrown`], which the function turns into its own error result.

use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use jni_sys::{
    JNI_EDETACHED, JNI_OK, JNI_VERSION_1_6, JNIEnv, JavaVM, jclass, jint, jmethodID, jobject,
    jsize, jstring, jvalue,
};

/// A Java exception happened and was cleared.
#[derive(Debug)]
pub struct Thrown;

static VM: AtomicPtr<JavaVM> = AtomicPtr::new(std::ptr::null_mut());

/// The process' VM: from an environment app code passed, or from the VM's own export.
fn vm() -> Option<*mut JavaVM> {
    let known = VM.load(Ordering::Acquire);
    if !known.is_null() {
        return Some(known);
    }
    type GetCreated = unsafe extern "system" fn(*mut *mut JavaVM, jsize, *mut jsize) -> jint;
    // SAFETY: the name is a C string; the symbol, when present, has JNI's documented signature.
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"JNI_GetCreatedJavaVMs".as_ptr());
        if symbol.is_null() {
            return None;
        }
        let get: GetCreated = std::mem::transmute(symbol);
        let mut vm = std::ptr::null_mut();
        let mut count = 0;
        if get(&mut vm, 1, &mut count) != JNI_OK || count < 1 || vm.is_null() {
            return None;
        }
        VM.store(vm, Ordering::Release);
        Some(vm)
    }
}

/// Detaches a thread this module attached when the thread exits, as ART requires.
struct Attached(*mut JavaVM);

impl Drop for Attached {
    fn drop(&mut self) {
        // SAFETY: the VM lives as long as the process, and this thread attached itself.
        unsafe { ((**self.0).v1_1.DetachCurrentThread)(self.0) };
    }
}

thread_local! {
    static ATTACHED: std::cell::RefCell<Option<Attached>> = const { std::cell::RefCell::new(None) };
}

/// A thread's JNI environment.
#[derive(Clone, Copy)]
pub struct Jni(*mut JNIEnv);

impl Jni {
    /// The environment app code passed in, which also tells which VM the process runs.
    ///
    /// # Safety
    /// `env` must be the calling thread's JNI environment.
    pub unsafe fn from_env(env: *mut JNIEnv) -> Jni {
        if VM.load(Ordering::Acquire).is_null() {
            let mut vm = std::ptr::null_mut();
            // SAFETY: as above.
            if unsafe { ((**env).v1_1.GetJavaVM)(env, &mut vm) } == JNI_OK {
                VM.store(vm, Ordering::Release);
            }
        }
        Jni(env)
    }

    /// The calling thread's environment, if it is attached to the VM.
    pub fn attached() -> Option<Jni> {
        let vm = vm()?;
        let mut env: *mut c_void = std::ptr::null_mut();
        // SAFETY: the VM lives as long as the process.
        let status = unsafe { ((**vm).v1_2.GetEnv)(vm, &mut env, JNI_VERSION_1_6) };
        (status == JNI_OK && !env.is_null()).then_some(Jni(env.cast()))
    }

    /// The calling thread's environment, attaching a native thread as a daemon when needed.
    pub fn current() -> Option<Jni> {
        if let Some(jni) = Jni::attached() {
            return Some(jni);
        }
        let vm = vm()?;
        let mut env: *mut c_void = std::ptr::null_mut();
        // SAFETY: the VM lives as long as the process. The thread is not attached: GetEnv said so.
        let status = unsafe {
            let status = ((**vm).v1_2.GetEnv)(vm, &mut env, JNI_VERSION_1_6);
            if status != JNI_EDETACHED {
                return None;
            }
            ((**vm).v1_4.AttachCurrentThreadAsDaemon)(vm, &mut env, std::ptr::null_mut())
        };
        if status != JNI_OK || env.is_null() {
            return None;
        }
        ATTACHED.with(|attached| *attached.borrow_mut() = Some(Attached(vm)));
        Some(Jni(env.cast()))
    }

    pub fn env(self) -> *mut JNIEnv {
        self.0
    }

    fn table(&self) -> &jni_sys::JNINativeInterface_ {
        // SAFETY: a Jni only holds a live environment of the calling thread.
        unsafe { &**self.0 }
    }

    /// Clears a pending exception, printing it first.
    pub fn check(self) -> Result<(), Thrown> {
        let table = self.table();
        // SAFETY: the calling thread's environment.
        unsafe {
            if (table.v1_2.ExceptionCheck)(self.0) {
                (table.v1_1.ExceptionDescribe)(self.0);
                (table.v1_1.ExceptionClear)(self.0);
                return Err(Thrown);
            }
        }
        Ok(())
    }

    /// Clears a pending exception the caller expects, such as a missing asset, without printing.
    pub fn quietly(self) -> bool {
        let table = self.table();
        // SAFETY: the calling thread's environment.
        unsafe {
            let pending = (table.v1_2.ExceptionCheck)(self.0);
            if pending {
                (table.v1_1.ExceptionClear)(self.0);
            }
            pending
        }
    }

    pub fn find_class(self, name: &CStr) -> Result<jclass, Thrown> {
        // SAFETY: the calling thread's environment and a C string.
        let class = unsafe { (self.table().v1_1.FindClass)(self.0, name.as_ptr()) };
        self.check()?;
        if class.is_null() {
            Err(Thrown)
        } else {
            Ok(class)
        }
    }

    fn method(self, object: jobject, name: &CStr, signature: &CStr) -> Result<jmethodID, Thrown> {
        let table = self.table();
        // SAFETY: the calling thread's environment, a live object and C strings.
        let id = unsafe {
            let class = (table.v1_1.GetObjectClass)(self.0, object);
            let id = (table.v1_1.GetMethodID)(self.0, class, name.as_ptr(), signature.as_ptr());
            (table.v1_1.DeleteLocalRef)(self.0, class);
            id
        };
        self.check()?;
        if id.is_null() { Err(Thrown) } else { Ok(id) }
    }

    /// Calls an instance method; the signature's return type picks the JNI call.
    ///
    /// # Safety
    /// `object` must be a live reference, and `arguments` must match `signature`.
    pub unsafe fn call(
        self,
        object: jobject,
        name: &CStr,
        signature: &CStr,
        arguments: &[jvalue],
    ) -> Result<jvalue, Thrown> {
        // SAFETY: as above.
        unsafe { self.invoke(object, name, signature, arguments, true) }
    }

    /// As [`Jni::call`], for an exception the caller expects, such as a missing asset.
    ///
    /// # Safety
    /// As [`Jni::call`].
    pub unsafe fn call_quietly(
        self,
        object: jobject,
        name: &CStr,
        signature: &CStr,
        arguments: &[jvalue],
    ) -> Result<jvalue, Thrown> {
        // SAFETY: as above.
        unsafe { self.invoke(object, name, signature, arguments, false) }
    }

    /// # Safety
    /// As [`Jni::call`].
    unsafe fn invoke(
        self,
        object: jobject,
        name: &CStr,
        signature: &CStr,
        arguments: &[jvalue],
        print: bool,
    ) -> Result<jvalue, Thrown> {
        if object.is_null() {
            return Err(Thrown);
        }
        let id = self.method(object, name, signature)?;
        let table = self.table();
        let args = arguments.as_ptr();
        let returns = signature.to_bytes().iter().rposition(|&b| b == b')');
        let kind = returns
            .and_then(|i| signature.to_bytes().get(i + 1))
            .copied();
        // SAFETY: as above; the call matches the method's return type.
        let value = unsafe {
            match kind {
                Some(b'V') => {
                    (table.v1_1.CallVoidMethodA)(self.0, object, id, args);
                    jvalue { j: 0 }
                }
                Some(b'Z') => jvalue {
                    z: (table.v1_1.CallBooleanMethodA)(self.0, object, id, args),
                },
                Some(b'I') => jvalue {
                    i: (table.v1_1.CallIntMethodA)(self.0, object, id, args),
                },
                Some(b'J') => jvalue {
                    j: (table.v1_1.CallLongMethodA)(self.0, object, id, args),
                },
                Some(b'L' | b'[') => jvalue {
                    l: (table.v1_1.CallObjectMethodA)(self.0, object, id, args),
                },
                _ => return Err(Thrown),
            }
        };
        if print {
            self.check()?;
        } else if self.quietly() {
            return Err(Thrown);
        }
        Ok(value)
    }

    /// As [`Jni::call`], for a static method; JNI ignores Java access, so private ones work too.
    ///
    /// # Safety
    /// `class` must be a live class reference, and `arguments` must match `signature`.
    pub unsafe fn call_static(
        self,
        class: jclass,
        name: &CStr,
        signature: &CStr,
        arguments: &[jvalue],
    ) -> Result<jvalue, Thrown> {
        let table = self.table();
        // SAFETY: as above.
        let id = unsafe {
            (table.v1_1.GetStaticMethodID)(self.0, class, name.as_ptr(), signature.as_ptr())
        };
        self.check()?;
        if id.is_null() {
            return Err(Thrown);
        }
        let args = arguments.as_ptr();
        let returns = signature.to_bytes().iter().rposition(|&b| b == b')');
        let kind = returns
            .and_then(|i| signature.to_bytes().get(i + 1))
            .copied();
        // SAFETY: as above; the call matches the method's return type.
        let value = unsafe {
            match kind {
                Some(b'V') => {
                    (table.v1_1.CallStaticVoidMethodA)(self.0, class, id, args);
                    jvalue { j: 0 }
                }
                Some(b'I') => jvalue {
                    i: (table.v1_1.CallStaticIntMethodA)(self.0, class, id, args),
                },
                Some(b'Z') => jvalue {
                    z: (table.v1_1.CallStaticBooleanMethodA)(self.0, class, id, args),
                },
                Some(b'L' | b'[') => jvalue {
                    l: (table.v1_1.CallStaticObjectMethodA)(self.0, class, id, args),
                },
                _ => return Err(Thrown),
            }
        };
        self.check()?;
        Ok(value)
    }

    /// # Safety
    /// `arguments` must match `signature`.
    pub unsafe fn new_object(
        self,
        class: &CStr,
        signature: &CStr,
        arguments: &[jvalue],
    ) -> Result<jobject, Thrown> {
        let class = self.find_class(class)?;
        let table = self.table();
        // SAFETY: as above, with a live class.
        let object = unsafe {
            let id =
                (table.v1_1.GetMethodID)(self.0, class, c"<init>".as_ptr(), signature.as_ptr());
            let object = if id.is_null() {
                std::ptr::null_mut()
            } else {
                (table.v1_1.NewObjectA)(self.0, class, id, arguments.as_ptr())
            };
            (table.v1_1.DeleteLocalRef)(self.0, class);
            object
        };
        self.check()?;
        if object.is_null() {
            Err(Thrown)
        } else {
            Ok(object)
        }
    }

    /// # Safety
    /// `class` must be a live class reference.
    pub unsafe fn static_object(
        self,
        class: jclass,
        name: &CStr,
        signature: &CStr,
    ) -> Result<jobject, Thrown> {
        let table = self.table();
        // SAFETY: as above.
        let value = unsafe {
            let id =
                (table.v1_1.GetStaticFieldID)(self.0, class, name.as_ptr(), signature.as_ptr());
            if id.is_null() {
                std::ptr::null_mut()
            } else {
                (table.v1_1.GetStaticObjectField)(self.0, class, id)
            }
        };
        self.check()?;
        Ok(value)
    }

    /// Reads an `int` or `long` field, private or not, widened to `i64`.
    ///
    /// # Safety
    /// `object` must be a live reference.
    pub unsafe fn field(
        self,
        object: jobject,
        name: &CStr,
        signature: &CStr,
    ) -> Result<i64, Thrown> {
        let table = self.table();
        // SAFETY: as above; the getter matches the signature.
        let value = unsafe {
            let class = (table.v1_1.GetObjectClass)(self.0, object);
            let id = (table.v1_1.GetFieldID)(self.0, class, name.as_ptr(), signature.as_ptr());
            (table.v1_1.DeleteLocalRef)(self.0, class);
            if id.is_null() {
                None
            } else if signature == c"J" {
                Some((table.v1_1.GetLongField)(self.0, object, id))
            } else {
                Some(i64::from((table.v1_1.GetIntField)(self.0, object, id)))
            }
        };
        self.check()?;
        value.ok_or(Thrown)
    }

    /// # Safety
    /// `object` must be a live `java.io.FileDescriptor`.
    pub unsafe fn set_int_field(
        self,
        object: jobject,
        name: &CStr,
        value: jint,
    ) -> Result<(), Thrown> {
        let table = self.table();
        // SAFETY: as above.
        unsafe {
            let class = (table.v1_1.GetObjectClass)(self.0, object);
            let id = (table.v1_1.GetFieldID)(self.0, class, name.as_ptr(), c"I".as_ptr());
            (table.v1_1.DeleteLocalRef)(self.0, class);
            if !id.is_null() {
                (table.v1_1.SetIntField)(self.0, object, id, value);
            }
        }
        self.check()
    }

    /// A global reference to `object`, which any thread may use until [`Jni::delete_global`].
    pub fn global(self, object: jobject) -> jobject {
        if object.is_null() {
            return object;
        }
        // SAFETY: the calling thread's environment and a live reference.
        unsafe { (self.table().v1_1.NewGlobalRef)(self.0, object) }
    }

    pub fn delete_global(self, object: jobject) {
        if !object.is_null() {
            // SAFETY: the caller owns this global reference.
            unsafe { (self.table().v1_1.DeleteGlobalRef)(self.0, object) }
        }
    }

    pub fn delete_local(self, object: jobject) {
        if !object.is_null() {
            // SAFETY: the caller owns this local reference.
            unsafe { (self.table().v1_1.DeleteLocalRef)(self.0, object) }
        }
    }

    pub fn same(self, first: jobject, second: jobject) -> bool {
        // SAFETY: the calling thread's environment and live references.
        unsafe { (self.table().v1_1.IsSameObject)(self.0, first, second) }
    }

    pub fn string(self, text: &CStr) -> Result<jstring, Thrown> {
        // SAFETY: the calling thread's environment and a C string.
        let string = unsafe { (self.table().v1_1.NewStringUTF)(self.0, text.as_ptr()) };
        self.check()?;
        if string.is_null() {
            Err(Thrown)
        } else {
            Ok(string)
        }
    }

    /// A Java string's modified UTF-8 bytes.
    pub fn utf(self, string: jstring) -> Option<Vec<u8>> {
        if string.is_null() {
            return None;
        }
        let table = self.table();
        // SAFETY: the calling thread's environment and a live string; copied before release.
        unsafe {
            let chars = (table.v1_1.GetStringUTFChars)(self.0, string, std::ptr::null_mut());
            if chars.is_null() {
                self.quietly();
                return None;
            }
            let bytes = CStr::from_ptr(chars).to_bytes().to_vec();
            (table.v1_1.ReleaseStringUTFChars)(self.0, string, chars);
            Some(bytes)
        }
    }

    pub fn int_array(self, length: usize) -> Result<jni_sys::jintArray, Thrown> {
        let length = jsize::try_from(length).map_err(|_| Thrown)?;
        // SAFETY: the calling thread's environment.
        let array = unsafe { (self.table().v1_1.NewIntArray)(self.0, length) };
        self.check()?;
        if array.is_null() {
            Err(Thrown)
        } else {
            Ok(array)
        }
    }

    /// Copies the first `into.len()` ints of `array` out.
    pub fn read_ints(self, array: jni_sys::jintArray, into: &mut [i32]) -> Result<(), Thrown> {
        // SAFETY: the calling thread's environment; JNI checks the range and throws if it is wrong.
        unsafe {
            (self.table().v1_1.GetIntArrayRegion)(
                self.0,
                array,
                0,
                into.len() as jsize,
                into.as_mut_ptr(),
            )
        };
        self.check()
    }

    /// Copies `from` into the start of `array`.
    pub fn write_ints(self, array: jni_sys::jintArray, from: &[i32]) -> Result<(), Thrown> {
        // SAFETY: as above.
        unsafe {
            (self.table().v1_1.SetIntArrayRegion)(
                self.0,
                array,
                0,
                from.len() as jsize,
                from.as_ptr(),
            )
        };
        self.check()
    }

    pub fn byte_array(self, length: usize) -> Result<jni_sys::jbyteArray, Thrown> {
        let length = jsize::try_from(length).map_err(|_| Thrown)?;
        // SAFETY: the calling thread's environment.
        let array = unsafe { (self.table().v1_1.NewByteArray)(self.0, length) };
        self.check()?;
        if array.is_null() {
            Err(Thrown)
        } else {
            Ok(array)
        }
    }

    /// Copies the first `into.len()` bytes of `array` out.
    pub fn read_bytes(self, array: jni_sys::jbyteArray, into: &mut [u8]) -> Result<(), Thrown> {
        // SAFETY: the calling thread's environment; JNI checks the range and throws if it is wrong.
        unsafe {
            (self.table().v1_1.GetByteArrayRegion)(
                self.0,
                array,
                0,
                into.len() as jsize,
                into.as_mut_ptr().cast(),
            )
        };
        self.check()
    }

    /// Registers natives on a class of `uxdroid-system.jar`.
    ///
    /// # Safety
    /// Each function must match its JNI signature.
    pub unsafe fn register(
        self,
        class: &CStr,
        methods: &[(&'static CStr, &'static CStr, *mut c_void)],
    ) -> Result<(), Thrown> {
        let class = self.find_class(class)?;
        let table: Vec<jni_sys::JNINativeMethod> = methods
            .iter()
            .map(|(name, signature, function)| jni_sys::JNINativeMethod {
                name: name.as_ptr().cast_mut(),
                signature: signature.as_ptr().cast_mut(),
                fnPtr: *function,
            })
            .collect();
        // SAFETY: as above; the names outlive the call.
        let status = unsafe {
            let status = (self.table().v1_1.RegisterNatives)(
                self.0,
                class,
                table.as_ptr(),
                table.len() as jint,
            );
            (self.table().v1_1.DeleteLocalRef)(self.0, class);
            status
        };
        self.check()?;
        if status == JNI_OK {
            Ok(())
        } else {
            Err(Thrown)
        }
    }
}
