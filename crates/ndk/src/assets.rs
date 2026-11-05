//! `AAssetManager` and `AAsset` over the framework's own `android.content.res.AssetManager`.
//!
//! The Java object already resolves the app's APK, its split APKs and overlays through AOSP's
//! `AssetManager2`, and decompresses entries, so each asset is read through it into memory once, at
//! open. That costs a copy, but no second APK parser, and `AAsset_getBuffer` then needs no mapping.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Arc, Mutex};

use jni_sys::{JNIEnv, jobject, jvalue};

use crate::java::{Jni, Thrown};
use crate::live::Live;

const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
const SEEK_END: c_int = 2;
/// Read in chunks of this many bytes, a bound on the transient Java array.
const CHUNK: usize = 64 * 1024;

struct Manager {
    /// A global reference to the Java AssetManager.
    java: jobject,
}

// SAFETY: a global reference may be used from any thread.
unsafe impl Send for Manager {}
// SAFETY: as above; it is never mutated.
unsafe impl Sync for Manager {}

struct Asset {
    manager: Arc<Manager>,
    name: CString,
    data: Box<[u8]>,
    position: Mutex<usize>,
}

struct Dir {
    files: Vec<CString>,
    next: Mutex<usize>,
}

static MANAGERS: Live<Manager> = Live::new();
static ASSETS: Live<Asset> = Live::new();
static DIRS: Live<Dir> = Live::new();

/// `AAssetManager* AAssetManager_fromJava(JNIEnv*, jobject)`.
pub unsafe extern "C" fn manager_from_java(env: *mut JNIEnv, manager: jobject) -> *mut c_void {
    if env.is_null() || manager.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: app code passes its thread's environment, as the NDK requires.
    let jni = unsafe { Jni::from_env(env) };
    // The same Java object gives the same native manager, as on Android.
    if let Some(known) = MANAGERS.find(|known| jni.same(known.java, manager)) {
        return known;
    }
    MANAGERS.add(Arc::new(Manager {
        java: jni.global(manager),
    }))
}

fn read_all(jni: Jni, stream: jobject) -> Result<Vec<u8>, Thrown> {
    let array = jni.byte_array(CHUNK)?;
    let mut data = Vec::new();
    let mut chunk = vec![0; CHUNK];
    let result = loop {
        // SAFETY: a live InputStream and a byte array of CHUNK bytes.
        let read = match unsafe { jni.call(stream, c"read", c"([B)I", &[jvalue { l: array }]) } {
            // SAFETY: `read([B)I` returns an int.
            Ok(value) => unsafe { value.i },
            Err(thrown) => break Err(thrown),
        };
        if read < 0 {
            break Ok(data);
        }
        let read = (read as usize).min(CHUNK);
        if let Err(thrown) = jni.read_bytes(array, &mut chunk[..read]) {
            break Err(thrown);
        }
        data.extend_from_slice(&chunk[..read]);
    };
    jni.delete_local(array);
    result
}

fn open_asset(manager: &Manager, name: &CStr, mode: c_int) -> Result<Vec<u8>, Thrown> {
    let jni = Jni::current().ok_or(Thrown)?;
    let text = jni.string(name)?;
    // AASSET_MODE_* and AssetManager.ACCESS_* share their values.
    let args = [
        jvalue { l: text },
        jvalue {
            i: mode.clamp(0, 3),
        },
    ];
    // SAFETY: a live AssetManager; a missing asset throws, which is an ordinary answer here.
    let stream = unsafe {
        jni.call_quietly(
            manager.java,
            c"open",
            c"(Ljava/lang/String;I)Ljava/io/InputStream;",
            &args,
        )
    };
    jni.delete_local(text);
    // SAFETY: the method returns an object.
    let stream = unsafe { stream?.l };
    let data = read_all(jni, stream);
    // SAFETY: a live InputStream.
    let _ = unsafe { jni.call(stream, c"close", c"()V", &[]) };
    jni.delete_local(stream);
    data
}

/// `AAsset* AAssetManager_open(AAssetManager*, const char* filename, int mode)`.
pub unsafe extern "C" fn manager_open(
    manager: *mut c_void,
    name: *const c_char,
    mode: c_int,
) -> *mut c_void {
    let Some(manager) = MANAGERS.get(manager) else {
        return std::ptr::null_mut();
    };
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the NDK contract makes `name` a C string.
    let name = unsafe { CStr::from_ptr(name) }.to_owned();
    match open_asset(&manager, &name, mode) {
        Ok(data) => ASSETS.add(Arc::new(Asset {
            manager,
            name,
            data: data.into_boxed_slice(),
            position: Mutex::new(0),
        })),
        Err(Thrown) => std::ptr::null_mut(),
    }
}

/// `AAssetDir* AAssetManager_openDir(AAssetManager*, const char* dirName)`: the files directly in
/// the directory, without subdirectories, as Android lists them.
pub unsafe extern "C" fn manager_open_dir(
    manager: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let Some(manager) = MANAGERS.get(manager) else {
        return std::ptr::null_mut();
    };
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the NDK contract makes `name` a C string.
    let directory = unsafe { CStr::from_ptr(name) }.to_bytes().to_vec();
    let Some(jni) = Jni::current() else {
        return std::ptr::null_mut();
    };
    let entries = list(jni, &manager, &directory).unwrap_or_default();
    let files = entries
        .into_iter()
        .filter(|entry| {
            let mut child = directory.clone();
            if !child.is_empty() {
                child.push(b'/');
            }
            child.extend_from_slice(entry);
            // APKs hold no empty directories, so only a directory lists anything.
            list(jni, &manager, &child).is_ok_and(|inside| inside.is_empty())
        })
        .filter_map(|entry| CString::new(entry).ok())
        .collect();
    DIRS.add(Arc::new(Dir {
        files,
        next: Mutex::new(0),
    }))
}

fn list(jni: Jni, manager: &Manager, directory: &[u8]) -> Result<Vec<Vec<u8>>, Thrown> {
    let name = CString::new(directory).map_err(|_| Thrown)?;
    let text = jni.string(&name)?;
    // SAFETY: a live AssetManager and a string.
    let array = unsafe {
        jni.call_quietly(
            manager.java,
            c"list",
            c"(Ljava/lang/String;)[Ljava/lang/String;",
            &[jvalue { l: text }],
        )
    };
    jni.delete_local(text);
    // SAFETY: the method returns an object.
    let array = unsafe { array?.l };
    if array.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: a live String array of the calling thread.
    let entries = unsafe {
        let table = &**jni.env();
        let count = (table.v1_1.GetArrayLength)(jni.env(), array);
        (0..count)
            .filter_map(|index| {
                let entry = (table.v1_1.GetObjectArrayElement)(jni.env(), array, index);
                let bytes = jni.utf(entry);
                jni.delete_local(entry);
                bytes
            })
            .collect()
    };
    jni.delete_local(array);
    Ok(entries)
}

/// `const char* AAssetDir_getNextFileName(AAssetDir*)`: valid until the directory closes.
pub unsafe extern "C" fn dir_next(dir: *mut c_void) -> *const c_char {
    let Some(dir) = DIRS.get(dir) else {
        return std::ptr::null();
    };
    let mut next = dir.next.lock().unwrap_or_else(|p| p.into_inner());
    let Some(name) = dir.files.get(*next) else {
        return std::ptr::null();
    };
    *next += 1;
    // The Dir stays alive in DIRS until AAssetDir_close, which keeps the name valid.
    name.as_ptr()
}

pub unsafe extern "C" fn dir_rewind(dir: *mut c_void) {
    if let Some(dir) = DIRS.get(dir) {
        *dir.next.lock().unwrap_or_else(|p| p.into_inner()) = 0;
    }
}

pub unsafe extern "C" fn dir_close(dir: *mut c_void) {
    drop(DIRS.remove(dir));
}

pub unsafe extern "C" fn asset_close(asset: *mut c_void) {
    drop(ASSETS.remove(asset));
}

/// `int AAsset_read(AAsset*, void* buf, size_t count)`.
pub unsafe extern "C" fn asset_read(
    asset: *mut c_void,
    buffer: *mut c_void,
    count: usize,
) -> c_int {
    let Some(asset) = ASSETS.get(asset) else {
        return -1;
    };
    if buffer.is_null() && count != 0 {
        return -1;
    }
    let mut position = asset.position.lock().unwrap_or_else(|p| p.into_inner());
    let available = asset.data.len().saturating_sub(*position);
    let count = count.min(available).min(c_int::MAX as usize);
    // SAFETY: the NDK contract gives `buffer` room for `count` bytes; the source range is in bounds.
    unsafe {
        std::ptr::copy_nonoverlapping(asset.data.as_ptr().add(*position), buffer.cast(), count);
    }
    *position += count;
    count as c_int
}

/// `off64_t AAsset_seek64(AAsset*, off64_t offset, int whence)`; `off_t` is 64 bits too on x86_64.
pub unsafe extern "C" fn asset_seek(asset: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let Some(asset) = ASSETS.get(asset) else {
        return -1;
    };
    let mut position = asset.position.lock().unwrap_or_else(|p| p.into_inner());
    let base = match whence {
        SEEK_SET => 0,
        SEEK_CUR => *position as i64,
        SEEK_END => asset.data.len() as i64,
        _ => return -1,
    };
    match base.checked_add(offset) {
        Some(target) if (0..=asset.data.len() as i64).contains(&target) => {
            *position = target as usize;
            target
        }
        _ => -1,
    }
}

pub unsafe extern "C" fn asset_get_buffer(asset: *mut c_void) -> *const c_void {
    ASSETS
        .get(asset)
        .map_or(std::ptr::null(), |asset| asset.data.as_ptr().cast())
}

pub unsafe extern "C" fn asset_get_length(asset: *mut c_void) -> i64 {
    ASSETS
        .get(asset)
        .map_or(-1, |asset| asset.data.len() as i64)
}

pub unsafe extern "C" fn asset_get_remaining(asset: *mut c_void) -> i64 {
    ASSETS.get(asset).map_or(-1, |asset| {
        let position = *asset.position.lock().unwrap_or_else(|p| p.into_inner());
        asset.data.len().saturating_sub(position) as i64
    })
}

/// `int AAsset_isAllocated(AAsset*)`: every asset here is a heap copy.
pub unsafe extern "C" fn asset_is_allocated(asset: *mut c_void) -> c_int {
    c_int::from(ASSETS.get(asset).is_some())
}

/// `int AAsset_openFileDescriptor64(AAsset*, off64_t* outStart, off64_t* outLength)`: a descriptor
/// of the APK and the entry's range, which exists only for uncompressed entries, as on Android.
pub unsafe extern "C" fn asset_open_fd(
    asset: *mut c_void,
    start: *mut i64,
    length: *mut i64,
) -> c_int {
    let Some(asset) = ASSETS.get(asset) else {
        return -1;
    };
    if start.is_null() || length.is_null() {
        return -1;
    }
    let Some(jni) = Jni::current() else {
        return -1;
    };
    match open_fd(jni, &asset) {
        Ok((fd, offset, size)) => {
            // SAFETY: both pointers were checked above, and the NDK contract makes them writable.
            unsafe {
                *start = offset;
                *length = size;
            }
            fd
        }
        Err(Thrown) => -1,
    }
}

fn open_fd(jni: Jni, asset: &Asset) -> Result<(c_int, i64, i64), Thrown> {
    let text = jni.string(&asset.name)?;
    // SAFETY: a live AssetManager; a compressed entry throws, which is an ordinary answer here.
    let descriptor = unsafe {
        jni.call_quietly(
            asset.manager.java,
            c"openFd",
            c"(Ljava/lang/String;)Landroid/content/res/AssetFileDescriptor;",
            &[jvalue { l: text }],
        )
    };
    jni.delete_local(text);
    // SAFETY: the method returns an object.
    let descriptor = unsafe { descriptor?.l };
    // SAFETY: a live AssetFileDescriptor; the signatures are the framework's.
    let result = unsafe {
        (|| {
            let offset = jni.call(descriptor, c"getStartOffset", c"()J", &[])?.j;
            let size = jni.call(descriptor, c"getLength", c"()J", &[])?.j;
            let parcel = jni
                .call(
                    descriptor,
                    c"getParcelFileDescriptor",
                    c"()Landroid/os/ParcelFileDescriptor;",
                    &[],
                )?
                .l;
            let fd = jni.call(parcel, c"getFd", c"()I", &[]);
            jni.delete_local(parcel);
            let own = libc::dup(fd?.i);
            if own < 0 {
                Err(Thrown)
            } else {
                Ok((own, offset, size))
            }
        })()
    };
    // SAFETY: as above.
    let _ = unsafe { jni.call(descriptor, c"close", c"()V", &[]) };
    jni.delete_local(descriptor);
    result
}
