//! The translation spike: an arm64 NDK library loaded through the bridge and run by Dynarmic,
//! each case crossing the guest boundary another way. JNI goes through a host `JNIEnv` and
//! `JavaVM` of this test, called the way ART calls native methods.
//!
//! Needs the engine (`UXDROID_TRANSLATOR`, set in the `translation` service) and the NDK.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use uxdroid_bridge::guest::{host_function, jni_trampoline, missing_signatures};
use uxdroid_bridge::{Library, load};

fn engine_available() -> bool {
    let available = std::env::var_os("UXDROID_TRANSLATOR").is_some();
    if !available {
        eprintln!("skipped: UXDROID_TRANSLATOR is not set; run in the translation service");
    }
    available
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridge/tests/fixtures/arm64")
}

/// Builds the arm64 test library once per test process.
fn library() -> &'static Library {
    static LIBRARY: OnceLock<&'static Library> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run in the dev image");
        let clang = Path::new(&ndk)
            .join("toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang++");
        let out = std::env::temp_dir().join(format!("uxdroid-spike-{}", std::process::id()));
        std::fs::create_dir_all(&out).unwrap();
        let path = out.join("libspike.so");
        let status = Command::new(clang)
            .args([
                "-O2",
                "-fPIC",
                "-shared",
                "-static-libstdc++",
                "-Wl,--gc-sections",
            ])
            .arg(fixture_dir().join("spike.cpp"))
            .arg("-llog")
            .arg("-o")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        uxdroid_translate::install();
        let library = load(&path, &[]).unwrap_or_else(|error| panic!("{error}"));
        eprintln!(
            "guest imports without a signature: {:?}",
            missing_signatures()
        );
        library
    })
}

/// A typed host pointer to a guest function.
fn function<F: Copy>(name: &str, signature: &str) -> F {
    assert_eq!(size_of::<F>(), size_of::<usize>());
    let address = library().symbol(name).expect(name) as u64;
    let thunk = host_function(address, signature).unwrap();
    // SAFETY: F is the function pointer type matching `signature`.
    unsafe { std::mem::transmute_copy(&thunk) }
}

#[test]
fn integer_arguments_and_results_cross() {
    if !engine_available() {
        return;
    }
    let add: extern "C" fn(i32, i32) -> i32 = function("spike_add", "i32(i32,i32)");
    assert_eq!(add(2, 3), 5);
    assert_eq!(add(-7, 3), -4);
}

#[test]
fn registers_and_stack_arguments_of_both_classes_cross() {
    if !engine_available() {
        return;
    }
    type Mix = extern "C" fn(
        i32,
        f64,
        i64,
        f32,
        i32,
        i32,
        i32,
        i32,
        i32,
        i32,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
    ) -> f64;
    let mix: Mix = function(
        "spike_mix",
        "f64(i32,f64,i64,f32,i32,i32,i32,i32,i32,i32,f64,f64,f64,f64,f64,f64,f64,f64)",
    );
    let expected = 1.0
        + 0.5
        + 3.0
        + 0.25
        + 5.0
        + 6.0
        + 7.0
        + 8.0
        + 90.0
        + 1000.0
        + 0.125
        + 1.0
        + 2.0
        + 3.0
        + 4.0
        + 5.0
        + 6000.0
        + 70000.0;
    let result = mix(
        1, 0.5, 3, 0.25, 5, 6, 7, 8, 9, 10, 0.125, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0,
    );
    assert_eq!(result, expected);
}

#[test]
fn a_host_qsort_calls_the_guest_comparator() {
    if !engine_available() {
        return;
    }
    let sort: extern "C" fn() -> i32 = function("spike_qsort", "i32()");
    assert_eq!(sort(), 1_235_789);
}

#[test]
fn a_guest_thread_runs_with_its_own_thread_local() {
    if !engine_available() {
        return;
    }
    let thread: extern "C" fn(i32) -> i32 = function("spike_thread", "i32(i32)");
    // The worker returns 21 * 2 + its fresh counter (0) + 5; the main thread's stays at 100.
    assert_eq!(thread(21), 47 + 100_000);
}

#[test]
fn a_cxx_exception_is_thrown_and_caught_in_guest_code() {
    if !engine_available() {
        return;
    }
    let throw: extern "C" fn(i32) -> i32 = function("spike_exception", "i32(i32)");
    assert_eq!(throw(0), 0);
    assert_eq!(throw(3), "guest exception".len() as i32 + 3);
}

#[test]
fn variadic_host_calls_follow_the_format() {
    if !engine_available() {
        return;
    }
    let format: extern "C" fn(*mut c_char, usize) -> i32 = function("spike_format", "i32(ptr,u64)");
    let mut buffer = [0 as c_char; 128];
    let written = format(buffer.as_mut_ptr(), buffer.len());
    // SAFETY: snprintf terminated the buffer.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().unwrap();
    assert_eq!(
        text,
        "int 42, string guest, double 2.50, long 1234567890123"
    );
    assert_eq!(written as usize, text.len());
}

/// Runs in a child process, so its log line can be read from the child's stderr.
#[test]
fn guest_log_child() {
    if std::env::var_os("UXDROID_SPIKE_CHILD").is_none() || !engine_available() {
        return;
    }
    let log: extern "C" fn() -> i32 = function("spike_log", "i32()");
    assert!(log() > 0);
}

#[test]
fn android_log_print_reaches_the_log() {
    if !engine_available() {
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "guest_log_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("UXDROID_SPIKE_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("spike: int 42, string guest, double 2.50, long 1234567890123"),
        "{stderr}"
    );
}

// --- A host JNI for the guest library, as ART would provide ---

#[repr(C)]
struct Env {
    functions: *const usize,
}

#[repr(C)]
struct Vm {
    functions: *const usize,
}

static REGISTERED: Mutex<Vec<(String, String, usize)>> = Mutex::new(Vec::new());
const CLASS: usize = 0x5ec1a55;

extern "C" fn get_env(_vm: *mut Vm, env: *mut *mut Env, _version: i32) -> i32 {
    // SAFETY: the caller passes a writable pointer.
    unsafe { *env = host_env() };
    0
}

extern "C" fn find_class(_env: *mut Env, name: *const c_char) -> usize {
    // SAFETY: JNI passes a C string.
    let name = unsafe { CStr::from_ptr(name) };
    if name.to_bytes() == b"dev/uxdroid/Spike" {
        CLASS
    } else {
        0
    }
}

extern "C" fn register_natives(
    _env: *mut Env,
    class: usize,
    methods: *const [usize; 3],
    count: i32,
) -> i32 {
    assert_eq!(class, CLASS);
    let mut registered = REGISTERED.lock().unwrap();
    for index in 0..count as usize {
        // SAFETY: JNI passes `count` records of C strings and a function.
        let [name, signature, function] = unsafe { *methods.add(index) };
        // SAFETY: as above.
        let text = |pointer: usize| {
            unsafe { CStr::from_ptr(pointer as *const c_char) }
                .to_string_lossy()
                .into_owned()
        };
        registered.push((text(name), text(signature), function));
    }
    0
}

/// Strings are C strings here: a `jstring` is the address of one.
extern "C" fn get_string_utf_chars(_env: *mut Env, string: usize, _copy: *mut u8) -> usize {
    string
}

extern "C" fn release_string_utf_chars(_env: *mut Env, _string: usize, _chars: usize) {}

extern "C" fn new_string_utf(_env: *mut Env, chars: *const c_char) -> usize {
    // SAFETY: JNI passes a C string.
    let text = unsafe { CStr::from_ptr(chars) }.to_owned();
    CString::into_raw(text) as usize
}

fn host_env() -> *mut Env {
    static ENV: OnceLock<usize> = OnceLock::new();
    *ENV.get_or_init(|| {
        let mut table = vec![0usize; 233];
        table[6] = find_class as *const () as usize;
        table[167] = new_string_utf as *const () as usize;
        table[169] = get_string_utf_chars as *const () as usize;
        table[170] = release_string_utf_chars as *const () as usize;
        table[215] = register_natives as *const () as usize;
        let functions = Box::leak(table.into_boxed_slice()).as_ptr();
        Box::leak(Box::new(Env { functions })) as *mut Env as usize
    }) as *mut Env
}

fn host_vm() -> *mut Vm {
    static VM: OnceLock<usize> = OnceLock::new();
    *VM.get_or_init(|| {
        let mut table = vec![0usize; 8];
        table[6] = get_env as *const () as usize;
        let functions = Box::leak(table.into_boxed_slice()).as_ptr();
        Box::leak(Box::new(Vm { functions })) as *mut Vm as usize
    }) as *mut Vm
}

fn registered(name: &str) -> (String, usize) {
    let registered = REGISTERED.lock().unwrap();
    let (_, signature, function) = registered
        .iter()
        .find(|(known, _, _)| known == name)
        .unwrap_or_else(|| panic!("{name} was not registered"));
    (signature.clone(), *function)
}

fn on_load() -> i32 {
    static RESULT: OnceLock<i32> = OnceLock::new();
    *RESULT.get_or_init(|| {
        let trampoline = jni_trampoline(library(), "JNI_OnLoad", None).unwrap();
        // SAFETY: ART's JNI_OnLoad signature.
        let on_load: extern "C" fn(*mut Vm, *mut c_void) -> i32 =
            unsafe { std::mem::transmute(trampoline) };
        on_load(host_vm(), std::ptr::null_mut())
    })
}

#[test]
fn jni_on_load_registers_natives_through_the_guest_env() {
    if !engine_available() {
        return;
    }
    const JNI_VERSION_1_6: i32 = 0x0001_0006;
    assert_eq!(on_load(), JNI_VERSION_1_6);
    assert_eq!(registered("add").0, "(II)I");
    assert_eq!(registered("mix").0, "(IJDFIIIIID)D");
    assert_eq!(
        registered("greet").0,
        "(Ljava/lang/String;)Ljava/lang/String;"
    );
}

#[test]
fn registered_natives_take_host_calls_by_shorty() {
    if !engine_available() {
        return;
    }
    on_load();
    let env = host_env();
    let class = CLASS;

    // SAFETY: each wrapper has the C signature of its JNI descriptor.
    unsafe {
        let add: extern "C" fn(*mut Env, usize, i32, i32) -> i32 =
            std::mem::transmute(registered("add").1);
        assert_eq!(add(env, class, 40, 2), 42);

        type Mix =
            extern "C" fn(*mut Env, usize, i32, i64, f64, f32, i32, i32, i32, i32, i32, f64) -> f64;
        let mix: Mix = std::mem::transmute(registered("mix").1);
        let result = mix(env, class, 1, 2, 0.5, 0.25, 3, 4, 5, 6, 7, 0.125);
        assert_eq!(
            result,
            1.0 + 2.0 + 0.5 + 0.25 + 3.0 + 4.0 + 5.0 + 6.0 + 70.0 + 12.5
        );

        let greet: extern "C" fn(*mut Env, usize, usize) -> usize =
            std::mem::transmute(registered("greet").1);
        let name = CString::new("arm64").unwrap();
        let greeting = greet(env, class, name.as_ptr() as usize);
        assert_eq!(
            CStr::from_ptr(greeting as *const c_char).to_str().unwrap(),
            "hello arm64"
        );
    }
}

#[test]
fn natives_found_by_name_use_the_shorty_art_passes() {
    if !engine_available() {
        return;
    }
    let trampoline =
        jni_trampoline(library(), "Java_dev_uxdroid_Spike_scale", Some(b"JJI")).unwrap();
    // SAFETY: the C signature of shorty JJI.
    let scale: extern "C" fn(*mut Env, usize, i64, i32) -> i64 =
        unsafe { std::mem::transmute(trampoline) };
    assert_eq!(scale(host_env(), CLASS, 6_000_000_000, 7), 42_000_000_000);
}
