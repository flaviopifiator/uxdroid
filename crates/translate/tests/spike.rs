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

use uxdroid_bridge::guest::{host_function, jni_trampoline, missing_signatures, profile};
use uxdroid_bridge::table::{Export, Table};
use uxdroid_bridge::{Library, load, table};

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

/// A new directory only this user can write, never one that existed: a name another user
/// could have made first would let them swap the library the test loads.
fn private_dir() -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let mut random = [0u8; 8];
    // SAFETY: writes at most 8 bytes into `random`.
    let read = unsafe { libc::getrandom(random.as_mut_ptr().cast(), 8, 0) };
    assert_eq!(read, 8);
    let out = std::env::temp_dir().join(format!(
        "uxdroid-spike-{}-{:016x}",
        std::process::id(),
        u64::from_le_bytes(random)
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&out).unwrap();
    out
}

/// The directory the test library was built into and loaded from.
static LIBRARY_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Taken by the cases that watch translations, retire the code cache or translate ahead: each
/// would see the others' translations in its counts.
static ENGINE_COUNTS: Mutex<()> = Mutex::new(());

fn engine_counts() -> std::sync::MutexGuard<'static, ()> {
    ENGINE_COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Builds the arm64 test library once per test process.
fn library() -> &'static Library {
    static LIBRARY: OnceLock<&'static Library> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        let ndk = std::env::var_os("ANDROID_NDK_ROOT").expect("run in the dev image");
        let clang = Path::new(&ndk)
            .join("toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang++");
        let out = private_dir();
        let _ = LIBRARY_DIR.set(out.clone());
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
        for fake in [&FAKE_EGL, &FAKE_GLES, &FAKE_ANDROID] {
            table::provide(fake);
        }
        let library = load(&path, &[]).unwrap_or_else(|error| panic!("{error}"));
        eprintln!(
            "guest imports without a signature: {:?}",
            missing_signatures()
        );
        library
    })
}

// --- System library tables of the test's, standing in for crates/ndk's ---

extern "C" fn fake_get_proc_address(_name: *const c_char) -> usize {
    // The host's eglGetProcAddress; the bridge answers guest code from the tables instead.
    0
}

extern "C" fn fake_multiply(a: i32, b: i32) -> i32 {
    a * b
}

/// Calls the callback at once, as a looper would when the descriptor is ready.
extern "C" fn fake_add_fd(
    _looper: usize,
    fd: i32,
    _ident: i32,
    events: i32,
    callback: extern "C" fn(i32, i32, usize) -> i32,
    data: usize,
) -> i32 {
    callback(fd, events, data)
}

static FAKE_EGL: Table = Table {
    library: "libEGL.so",
    exports: &[Export::function(
        c"eglGetProcAddress",
        "ptr(ptr)",
        fake_get_proc_address as *const (),
    )],
};

extern "C" fn fake_retire_code_cache() {
    assert!(uxdroid_translate::retire_code_cache());
}

static FAKE_GLES: Table = Table {
    library: "libGLESv2.so",
    exports: &[
        Export::function(
            c"glUxdroidMultiply",
            "i32(i32,i32)",
            fake_multiply as *const (),
        ),
        Export::function(
            c"glUxdroidRetireCodeCache",
            "void()",
            fake_retire_code_cache as *const (),
        ),
    ],
};

/// 7 when called with the host's env, as the host's `AAssetManager_fromJava` must be.
extern "C" fn fake_asset_manager(env: usize, manager: usize) -> usize {
    match (env == host_env() as usize, manager) {
        (true, MANAGER) => 7,
        (false, _) => 1,
        (true, _) => 2,
    }
}

const MANAGER: usize = 0x5a5a;

static FAKE_ANDROID: Table = Table {
    library: "libandroid.so",
    exports: &[
        Export::function(
            c"ALooper_addFd",
            "i32(ptr,i32,i32,i32,ptr,ptr)",
            fake_add_fd as *const (),
        ),
        Export::function(
            c"AAssetManager_fromJava",
            "ptr(ptr,ptr)",
            fake_asset_manager as *const (),
        ),
    ],
};

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

/// What `spike_shared_code` computes, on the host.
fn shared_code(rounds: i32) -> i32 {
    let mut sum = 0i32;
    for i in 0..rounds {
        sum = sum.wrapping_add(i.wrapping_mul(7) ^ (sum >> 3));
    }
    sum
}

/// The only case that watches or retires: the counts below would see other cases' doing.
#[test]
fn code_translated_once_is_run_by_every_thread_until_its_cache_retires() {
    if !engine_available() {
        return;
    }
    let _counts = engine_counts();
    let entry = library().symbol("spike_shared_code").unwrap() as u64;
    // A cache of its own: the profile cases may have translated this code already.
    assert!(uxdroid_translate::retire_code_cache());
    assert!(uxdroid_translate::watch_translations(entry));
    let run: extern "C" fn(i32) -> i32 = function("spike_shared_code", "i32(i32)");
    let expected = shared_code(1000);
    let translations = || uxdroid_translate::stats().unwrap().watched_translations;

    assert_eq!(
        std::thread::spawn(move || run(1000)).join().unwrap(),
        expected
    );
    assert_eq!(translations(), 1);

    // Eight threads at once, new to guest code, each with its own CPUs: none translates it again.
    let threads: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(move || (0..200).all(|_| run(1000) == expected)))
        .collect();
    for thread in threads {
        assert!(thread.join().unwrap());
    }
    assert_eq!(translations(), 1);

    // A retired cache takes no more code and a new one starts: the next thread entering guest
    // code runs from it, where the code is translated once more.
    let before = uxdroid_translate::stats().unwrap();
    assert!(uxdroid_translate::retire_code_cache());
    assert_eq!(
        std::thread::spawn(move || run(1000)).join().unwrap(),
        expected
    );
    let after = uxdroid_translate::stats().unwrap();
    assert!(after.caches > before.caches, "{before:?} then {after:?}");
    assert_eq!(after.watched_translations, 2);
    assert_eq!(run(1000), expected);

    // Retired while guest code runs, inside one of its host calls: the run stops at the first
    // block the old cache lacks, moves to the new cache and goes on at the same instruction.
    let mid_run: extern "C" fn(i32) -> i32 = function("spike_retire_mid_run", "i32(i32)");
    let caches = uxdroid_translate::stats().unwrap().caches;
    let mut sum = 1i32;
    for i in 0..500 {
        sum = sum.wrapping_mul(31).wrapping_add(i ^ 500);
    }
    assert_eq!(
        std::thread::spawn(move || mid_run(500)).join().unwrap(),
        sum
    );
    assert!(uxdroid_translate::stats().unwrap().caches > caches);
}

#[test]
fn a_saved_profile_translates_code_ahead_in_a_new_code_cache() {
    if !engine_available() {
        return;
    }
    let _counts = engine_counts();
    let entry = library().symbol("spike_shared_code").unwrap() as u64;
    let run: extern "C" fn(i32) -> i32 = function("spike_shared_code", "i32(i32)");
    assert_eq!(run(300), shared_code(300));
    let dir = LIBRARY_DIR.get().unwrap();
    let saved = profile::save(dir).unwrap();
    assert!(saved > 0);
    assert!(dir.join(profile::FILE).is_file());

    // As at the next start: a code cache that has translated nothing of the library yet.
    assert!(uxdroid_translate::watch_translations(entry));
    assert!(uxdroid_translate::retire_code_cache());
    let ahead = profile::pretranslate(library()).unwrap();
    assert!(ahead > 0 && ahead <= saved, "{ahead} of {saved}");
    let translations = || uxdroid_translate::stats().unwrap().watched_translations;
    assert_eq!(translations(), 1);
    // The code was ready before any thread ran it.
    assert_eq!(
        std::thread::spawn(move || run(300)).join().unwrap(),
        shared_code(300)
    );
    assert_eq!(translations(), 1);
}

#[test]
fn a_stale_profile_is_ignored() {
    if !engine_available() {
        return;
    }
    let _counts = engine_counts();
    let entry = library().symbol("spike_shared_code").unwrap() as u64;
    let run: extern "C" fn(i32) -> i32 = function("spike_shared_code", "i32(i32)");
    assert_eq!(run(300), shared_code(300));
    let dir = LIBRARY_DIR.get().unwrap();
    profile::save(dir).unwrap();
    let path = dir.join(profile::FILE);
    let saved = std::fs::read(&path).unwrap();

    // Made by another engine: bytes 8 to 16 hold the engine's identity.
    let mut other_engine = saved.clone();
    other_engine[8] ^= 0xff;
    std::fs::write(&path, &other_engine).unwrap();
    assert!(uxdroid_translate::watch_translations(entry));
    assert!(uxdroid_translate::retire_code_cache());
    let refused = profile::pretranslate(library()).unwrap_err();
    assert!(refused.ends_with("made by another engine"), "{refused}");
    assert_eq!(uxdroid_translate::stats().unwrap().watched_translations, 0);

    // For a library that changed since: a reinstall gives the file a new modification time.
    std::fs::write(&path, &saved).unwrap();
    let library_file = std::fs::File::options()
        .write(true)
        .open(dir.join("libspike.so"))
        .unwrap();
    library_file
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    let refused = profile::pretranslate(library()).unwrap_err();
    assert!(
        refused.ends_with("libspike.so has changed since it was made"),
        "{refused}"
    );
    assert_eq!(uxdroid_translate::stats().unwrap().watched_translations, 0);
}

/// The virtual address range of the test library's executable code, from its file.
fn spike_code_range() -> (u64, u64) {
    use uxdroid_bridge::elf::{PF_X, PHDR_SIZE, PT_LOAD, parse_header, parse_program_headers};
    let bytes = std::fs::read(LIBRARY_DIR.get().unwrap().join("libspike.so")).unwrap();
    let table = parse_header(&bytes).unwrap();
    let start = table.offset as usize;
    let headers = parse_program_headers(&bytes[start..start + table.count * PHDR_SIZE]).unwrap();
    let code = headers
        .iter()
        .find(|header| header.kind == PT_LOAD && header.flags & PF_X != 0)
        .unwrap();
    (code.vaddr, code.vaddr + code.filesz)
}

/// A saved profile with its blocks replaced by `entries`, each an offset and the engine's flags,
/// all in the test library, which is the only library in its directory. The format is the one
/// guest/profile.rs documents: magic, engine, libraries, then the count and 8-byte blocks.
fn profile_with(entries: &[(u32, u8)]) -> Vec<u8> {
    let dir = LIBRARY_DIR.get().unwrap();
    profile::save(dir).unwrap();
    let saved = std::fs::read(dir.join(profile::FILE)).unwrap();
    let word = |at: usize| u32::from_le_bytes(saved[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(word(16), 1, "one library in the test's directory");
    let blocks = 20 + 4 + word(20) + 16;
    let mut crafted = saved[..blocks].to_vec();
    crafted.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for &(offset, flags) in entries {
        crafted.extend_from_slice(&[0, 0, flags, 0]);
        crafted.extend_from_slice(&offset.to_le_bytes());
    }
    crafted
}

#[test]
fn profile_blocks_at_the_end_of_the_code_stop_at_its_end() {
    if !engine_available() {
        return;
    }
    let _counts = engine_counts();
    library();
    let (start, end) = spike_code_range();
    let end = u32::try_from(end).unwrap();
    // Starts in the last bytes of the code, aligned or not, whose decoding would run into the
    // unmapped gap after it, and a block of the code's own start for comparison.
    let crafted = profile_with(&[
        (end - 3, 0),
        (end - 1, 0),
        (end - 4, 0),
        (end - 8, 0),
        (u32::try_from(start).unwrap(), 0),
    ]);
    std::fs::write(LIBRARY_DIR.get().unwrap().join(profile::FILE), crafted).unwrap();
    assert!(uxdroid_translate::retire_code_cache());
    // Unaligned starts are refused; the aligned ones translate up to the end of the code.
    assert_eq!(profile::pretranslate(library()), Ok(3));
    let add: extern "C" fn(i32, i32) -> i32 = function("spike_add", "i32(i32,i32)");
    assert_eq!(add(2, 3), 5);
}

#[test]
fn a_profile_larger_than_any_valid_one_is_refused_unread() {
    if !engine_available() {
        return;
    }
    let _counts = engine_counts();
    library();
    let path = LIBRARY_DIR.get().unwrap().join(profile::FILE);
    // Sparse: the size is the point, not the bytes.
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64 << 20).unwrap();
    drop(file);
    let refused = profile::pretranslate(library()).unwrap_err();
    assert!(
        refused.contains("larger than any translation profile"),
        "{refused}"
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn guest_threads_add_to_one_counter_with_exclusive_stores() {
    if !engine_available() {
        return;
    }
    let count: extern "C" fn(i32, i32) -> i32 = function("spike_atomic_count", "i32(i32,i32)");
    assert_eq!(count(8, 20_000), 160_000);
}

#[test]
fn libdl_answers_a_guest_destructor_run_as_its_thread_exits() {
    if !engine_available() {
        return;
    }
    // glibc runs pthread key destructors after the host's thread_local values are destroyed.
    let exit: extern "C" fn() -> i32 = function("spike_thread_exit", "i32()");
    assert_eq!(exit(), 1);
}

#[test]
fn a_guest_thread_s_stack_bounds_hold_its_stack_pointer() {
    if !engine_available() {
        return;
    }
    let bounds: extern "C" fn() -> i32 = function("spike_stack_bounds", "i32()");
    assert_eq!(bounds(), 11);
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

#[test]
fn a_guest_va_list_is_read_from_its_save_areas_and_stack() {
    if !engine_available() {
        return;
    }
    let format: extern "C" fn(*mut c_char, usize) -> i32 =
        function("spike_vformat", "i32(ptr,u64)");
    let mut buffer = [0 as c_char; 160];
    let written = format(buffer.as_mut_ptr(), buffer.len());
    // SAFETY: vsnprintf terminated the buffer.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().unwrap();
    assert_eq!(
        text,
        "1 2 3 4 5 6 7 8 9000000000|0.5 1.5 2.5 3.5 4.5 5.5 6.5 7.5 8.5 9.5|end"
    );
    assert_eq!(written as usize, text.len());

    let allocate: extern "C" fn() -> *mut c_char = function("spike_vasprintf", "ptr()");
    let text = allocate();
    assert!(!text.is_null());
    // SAFETY: vasprintf made a C string with the process' malloc.
    unsafe {
        assert_eq!(CStr::from_ptr(text).to_str(), Ok("va-7-0.125"));
        libc::free(text.cast());
    }
}

/// A private directory with a 5-byte file, for the file system cases.
fn files() -> (CString, CString) {
    let dir = private_dir();
    let file = dir.join("five");
    std::fs::write(&file, b"12345").unwrap();
    let text = |path: &Path| CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    (text(&file), text(&dir))
}

#[test]
fn stat_fills_arm64_s_struct_stat() {
    if !engine_available() {
        return;
    }
    let stat: extern "C" fn(*const c_char, i32) -> i64 = function("spike_stat", "i64(ptr,i32)");
    let (file, _) = files();
    for which in 0..4 {
        // 5 bytes, a regular file, one link.
        assert_eq!(stat(file.as_ptr(), which), 5101, "call {which}");
    }
    let missing = CString::new("/nonexistent/uxdroid").unwrap();
    assert_eq!(stat(missing.as_ptr(), 0), -i64::from(libc::ENOENT));
}

#[test]
fn open_flags_are_translated_both_ways() {
    if !engine_available() {
        return;
    }
    let open: extern "C" fn(*const c_char, *const c_char) -> i32 =
        function("spike_open_flags", "i32(ptr,ptr)");
    let (file, dir) = files();
    assert_eq!(open(file.as_ptr(), dir.as_ptr()), 11111);
}

#[test]
fn epoll_events_and_ioctls_cross_in_arm64_layout() {
    if !engine_available() {
        return;
    }
    let epoll: extern "C" fn() -> i64 = function("spike_epoll", "i64()");
    assert_eq!(epoll(), 1111);
}

#[test]
fn longjmp_returns_to_setjmp_inside_guest_code() {
    if !engine_available() {
        return;
    }
    let jump: extern "C" fn(i32) -> i32 = function("spike_setjmp", "i32(i32)");
    assert_eq!(jump(7), 7 + 21);
}

#[test]
fn a_guest_signal_handler_runs_and_reads_back() {
    if !engine_available() {
        return;
    }
    let signal: extern "C" fn() -> i32 = function("spike_signal", "i32()");
    assert_eq!(signal(), 1000 + libc::SIGUSR1 * 10 + 1);
}

#[test]
fn a_guest_fault_signal_handler_is_refused() {
    if !engine_available() {
        return;
    }
    let fault: extern "C" fn() -> i32 = function("spike_fault_signal", "i32()");
    assert_eq!(fault(), 11);
}

#[test]
fn long_double_and_scanf_results_cross() {
    if !engine_available() {
        return;
    }
    let strtold: extern "C" fn(*const c_char) -> f64 = function("spike_strtold", "f64(ptr)");
    let text = CString::new("1.25").unwrap();
    assert_eq!(strtold(text.as_ptr()), 2.5);
    let sscanf: extern "C" fn(*const c_char) -> i32 = function("spike_sscanf", "i32(ptr)");
    let text = CString::new("1 2 3 4 5 6 7 8 9").unwrap();
    assert_eq!(sscanf(text.as_ptr()), 9045);
}

#[test]
fn guest_dynamic_linking_goes_through_the_bridge() {
    if !engine_available() {
        return;
    }
    let dl: extern "C" fn() -> i32 = function("spike_dl", "i32()");
    assert_eq!(dl(), 11111);
}

#[test]
fn host_function_pointers_and_callbacks_cross_as_guest_stubs_and_thunks() {
    if !engine_available() {
        return;
    }
    let proc_address: extern "C" fn() -> i32 = function("spike_proc_address", "i32()");
    assert_eq!(proc_address(), 1042);
    let looper: extern "C" fn() -> i32 = function("spike_looper", "i32()");
    assert_eq!(looper(), 511);
}

#[test]
fn guest_ifuncs_resolve_with_the_guest_hwcap() {
    if !engine_available() {
        return;
    }
    let exported: extern "C" fn() -> i32 = function("spike_ifunc", "i32()");
    assert_eq!(exported(), 8);
    let internal: extern "C" fn() -> i32 = function("spike_call_ifunc", "i32()");
    assert_eq!(internal(), 16);
}

#[test]
fn raw_system_calls_run_by_their_arm64_number() {
    if !engine_available() {
        return;
    }
    let getpid: extern "C" fn() -> i64 = function("spike_raw_getpid", "i64()");
    assert_eq!(getpid(), i64::from(std::process::id()));
    let unknown: extern "C" fn() -> i64 = function("spike_raw_unknown", "i64()");
    assert_eq!(unknown(), -i64::from(libc::ENOSYS));
}

/// Runs in a child process, so its log line can be read from the child's stderr.
#[test]
fn guest_log_child() {
    if std::env::var_os("UXDROID_SPIKE_CHILD").is_none() || !engine_available() {
        return;
    }
    let log: extern "C" fn() -> i32 = function("spike_log", "i32()");
    assert!(log() > 0);
    let vlog: extern "C" fn() -> i32 = function("spike_vlog", "i32()");
    assert!(vlog() > 0);
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
    assert!(stderr.contains("spike: vprint carried 64 0.5"), "{stderr}");
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

/// Method IDs of the test's Java class, by name.
const METHODS: [&str; 4] = ["sum", "twice", "triple", "<init>"];

extern "C" fn get_method_id(
    _env: *mut Env,
    class: usize,
    name: *const c_char,
    _sig: *const c_char,
) -> usize {
    assert_eq!(class, CLASS);
    // SAFETY: JNI passes a C string.
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap();
    METHODS
        .iter()
        .position(|known| *known == name)
        .map_or(0, |index| index + 1)
}

/// What the test's Java methods return, read from a `jvalue` array by each one's descriptor.
fn evaluate(method: usize, arguments: *const u64) -> f64 {
    // SAFETY: the bridge passes one jvalue per parameter of the method.
    let argument = |index: usize| unsafe { *arguments.add(index) };
    match METHODS[method - 1] {
        "sum" => {
            f64::from(argument(0) as i32)
                + argument(1) as i64 as f64
                + f64::from_bits(argument(2))
                + f64::from(f32::from_bits(argument(3) as u32))
                + f64::from(argument(4) as u8)
                + f64::from(argument(5) as i8)
                + f64::from(argument(6) as u16)
                + f64::from(argument(7) as i16)
        }
        "twice" => f64::from(argument(0) as i32 * 2),
        "triple" => (argument(0) as i64 * 3) as f64,
        "<init>" => f64::from(argument(0) as i32),
        other => panic!("{other}"),
    }
}

extern "C" fn call_double_a(
    _env: *mut Env,
    _this: usize,
    method: usize,
    arguments: *const u64,
) -> f64 {
    evaluate(method, arguments)
}

extern "C" fn call_int_a(
    _env: *mut Env,
    _this: usize,
    method: usize,
    arguments: *const u64,
) -> i32 {
    evaluate(method, arguments) as i32
}

extern "C" fn call_long_a(
    _env: *mut Env,
    _this: usize,
    method: usize,
    arguments: *const u64,
) -> i64 {
    evaluate(method, arguments) as i64
}

extern "C" fn call_nonvirtual_int_a(
    _env: *mut Env,
    _this: usize,
    class: usize,
    method: usize,
    arguments: *const u64,
) -> i32 {
    assert_eq!(class, CLASS);
    evaluate(method, arguments) as i32
}

/// A made object is the constructor's argument, which the guest adds to its total.
extern "C" fn new_object_a(
    _env: *mut Env,
    class: usize,
    method: usize,
    arguments: *const u64,
) -> usize {
    assert_eq!(class, CLASS);
    evaluate(method, arguments) as usize
}

fn host_env() -> *mut Env {
    static ENV: OnceLock<usize> = OnceLock::new();
    *ENV.get_or_init(|| {
        let mut table = vec![0usize; 233];
        table[6] = find_class as *const () as usize;
        table[30] = new_object_a as *const () as usize;
        table[33] = get_method_id as *const () as usize;
        table[51] = call_int_a as *const () as usize;
        table[60] = call_double_a as *const () as usize;
        table[81] = call_nonvirtual_int_a as *const () as usize;
        table[113] = get_method_id as *const () as usize;
        table[131] = call_int_a as *const () as usize;
        table[134] = call_long_a as *const () as usize;
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

#[test]
fn guest_calls_into_java_carry_every_argument_form() {
    if !engine_available() {
        return;
    }
    let trampoline =
        jni_trampoline(library(), "Java_dev_uxdroid_Spike_jniCalls", Some(b"D")).unwrap();
    // SAFETY: the C signature of shorty D on an instance method.
    let calls: extern "C" fn(*mut Env, usize) -> f64 = unsafe { std::mem::transmute(trampoline) };
    // sum 59.75, twice(21) by va_list, triple(14) static, twice(4) by jvalue, twice(5) nonvirtual,
    // and the object made from 9.
    assert_eq!(
        calls(host_env(), 0x0b1ec7),
        59.75 + 42.0 + 42.0 + 8.0 + 10.0 + 9.0
    );
}

#[test]
fn ndk_functions_taking_the_env_get_the_host_one() {
    if !engine_available() {
        return;
    }
    let trampoline =
        jni_trampoline(library(), "Java_dev_uxdroid_Spike_assets", Some(b"IL")).unwrap();
    // SAFETY: the C signature of shorty IL on a static method.
    let assets: extern "C" fn(*mut Env, usize, usize) -> i32 =
        unsafe { std::mem::transmute(trampoline) };
    assert_eq!(assets(host_env(), CLASS, MANAGER), 7);
}
