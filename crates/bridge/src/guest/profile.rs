//! Which guest code an app ran, kept beside its libraries, so the next start translates that code
//! ahead, on a thread of its own, instead of on each app thread as it first gets there.
//!
//! Translated code itself cannot be kept: Dynarmic's code holds absolute addresses, those of the
//! guest library, which is mapped somewhere else at each start, and those of the engine's own
//! dispatcher and callbacks. What can be decided once and kept is which blocks to translate: each
//! block's offset in its library and the engine's flags for it, in the order they were first
//! needed. That list is the profile, `arm64-v8a.profile` beside `arm64-v8a.plan.json`, in the app's
//! library directory, which `uxdroid install` fills.
//!
//! A profile names, for each library, its size and modification time, and the engine's identity.
//! A library whose file changed, or another engine, makes the profile stale, and it is not used:
//! the next save replaces it. Whatever the file says, only offsets inside a library's executable
//! segments are translated, so a damaged or crafted profile costs translation time, never safety.
//!
//! In an installed app, `library_loaded` starts both halves: a thread that translates each new
//! library's part of the profile as the library loads, and a thread that saves the profile 3, 10
//! and 30 seconds after the first guest library loaded, each time the engine has translated more.
//! `UXDROID_TRANSLATION_PROFILE=0` turns both off.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use crate::loader::{self, Library};

/// The profile's file name, beside the libraries.
pub const FILE: &str = "arm64-v8a.profile";
/// Marks a directory as an installed app's, the only kind whose profile is kept automatically.
pub const PLAN_FILE: &str = "arm64-v8a.plan.json";
/// A location's guest PC is its low 56 bits; the engine's flags for the block are above them.
pub const PC_BITS: u32 = 56;
const PC_MASK: u64 = (1 << PC_BITS) - 1;

const MAGIC: [u8; 8] = *b"UXDPROF1";
/// Bounds on what a profile may hold, far above any real app's, so a crafted file cannot make the
/// reader allocate without limit.
const MAX_LIBRARIES: usize = 1024;
const MAX_NAME: usize = 4096;
const MAX_ENTRIES: usize = 1 << 22;
/// When the profile is saved, counted from the first guest library's load.
const SAVE_AFTER: [Duration; 3] = [
    Duration::from_secs(3),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// What identifies a library's file: a reinstall or an update changes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    name: String,
    size: u64,
    modified_ns: u64,
}

impl Key {
    fn of(path: &Path, name: &str) -> Result<Key, String> {
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos() as u64);
        Ok(Key {
            name: name.to_string(),
            size: metadata.len(),
            modified_ns: modified,
        })
    }
}

/// One block: which library, where in it, and the engine's flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Entry {
    library: u16,
    flags: u8,
    offset: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Profile {
    engine: u64,
    libraries: Vec<Key>,
    entries: Vec<Entry>,
}

impl Profile {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.entries.len() * 8);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.engine.to_le_bytes());
        out.extend_from_slice(&(self.libraries.len() as u32).to_le_bytes());
        for key in &self.libraries {
            out.extend_from_slice(&(key.name.len() as u32).to_le_bytes());
            out.extend_from_slice(key.name.as_bytes());
            out.extend_from_slice(&key.size.to_le_bytes());
            out.extend_from_slice(&key.modified_ns.to_le_bytes());
        }
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for entry in &self.entries {
            out.extend_from_slice(&entry.library.to_le_bytes());
            out.push(entry.flags);
            out.push(0);
            out.extend_from_slice(&entry.offset.to_le_bytes());
        }
        out
    }

    /// Reads a profile, checking every length against the bytes and the bounds above.
    fn decode(bytes: &[u8]) -> Result<Profile, String> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(8)? != MAGIC {
            return Err("not a translation profile".to_string());
        }
        let engine = reader.u64()?;
        let count = reader.u32()? as usize;
        if count > MAX_LIBRARIES {
            return Err(format!("{count} libraries"));
        }
        let mut libraries = Vec::with_capacity(count);
        for _ in 0..count {
            let length = reader.u32()? as usize;
            if length > MAX_NAME {
                return Err("a library name too long".to_string());
            }
            let name = std::str::from_utf8(reader.take(length)?)
                .map_err(|_| "a library name that is not UTF-8")?
                .to_string();
            let size = reader.u64()?;
            let modified_ns = reader.u64()?;
            libraries.push(Key {
                name,
                size,
                modified_ns,
            });
        }
        let count = reader.u32()? as usize;
        if count > MAX_ENTRIES {
            return Err(format!("{count} blocks"));
        }
        if reader.bytes.len() - reader.at != count * 8 {
            return Err("its blocks do not fill the file".to_string());
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let raw = reader.take(8)?;
            let library = u16::from_le_bytes([raw[0], raw[1]]);
            if usize::from(library) >= libraries.len() {
                return Err("a block of a library it does not name".to_string());
            }
            entries.push(Entry {
                library,
                flags: raw[2],
                offset: u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
            });
        }
        Ok(Profile {
            engine,
            libraries,
            entries,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or("the file ends early")?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let raw = self.take(4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let raw = self.take(8)?;
        let mut word = [0u8; 8];
        word.copy_from_slice(raw);
        Ok(u64::from_le_bytes(word))
    }
}

fn directory_of(library: &Library) -> Option<PathBuf> {
    library
        .path()
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn libraries_in(directory: &Path) -> Vec<&'static Library> {
    loader::guest_libraries()
        .into_iter()
        .filter(|library| directory_of(library).as_deref() == Some(directory))
        .collect()
}

/// Writes what the engine has translated from the guest libraries loaded from `directory` as the
/// profile there, replacing any. Returns how many blocks it holds.
pub fn save(directory: &Path) -> Result<usize, String> {
    let engine = super::engine()?;
    let libraries = libraries_in(directory);
    let ranges: Vec<(u64, Vec<(u64, u64)>)> = libraries
        .iter()
        .map(|library| (library.bias(), library.code_ranges()))
        .collect();
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    let mut used = vec![false; libraries.len()];
    for location in engine.translated() {
        let pc = location & PC_MASK;
        let flags = (location >> PC_BITS) as u8;
        let found = ranges.iter().enumerate().find_map(|(index, (bias, code))| {
            let vaddr = pc.wrapping_sub(*bias);
            code.iter()
                .any(|&(start, end)| (start..end).contains(&vaddr))
                .then_some((index, vaddr))
        });
        let Some((index, vaddr)) = found else {
            continue;
        };
        let (Ok(library), Ok(offset)) = (u16::try_from(index), u32::try_from(vaddr)) else {
            continue;
        };
        let entry = Entry {
            library,
            flags,
            offset,
        };
        if seen.insert(entry) {
            used[index] = true;
            entries.push(entry);
        }
    }
    let mut keys = Vec::with_capacity(libraries.len());
    for library in &libraries {
        keys.push(Key::of(library.path(), library.name())?);
    }
    let profile = Profile {
        engine: engine.identity(),
        libraries: keys,
        entries,
    };
    let count = profile.entries.len();
    let path = directory.join(FILE);
    // A new file renamed over the old, so a reader never sees half of one.
    let partial = directory.join(format!(".{FILE}.{}", std::process::id()));
    std::fs::write(&partial, profile.encode())
        .and_then(|()| std::fs::rename(&partial, &path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            format!("{}: {error}", path.display())
        })?;
    Ok(count)
}

/// Translates ahead the blocks the profile beside `library` lists for it. Returns how many it
/// went through, or why the profile was not used: missing, unreadable, made by another engine,
/// or for a library file that has changed since.
pub fn pretranslate(library: &Library) -> Result<usize, String> {
    let directory = directory_of(library).ok_or("the library has no directory")?;
    let engine = super::engine()?;
    let path = directory.join(FILE);
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let profile = Profile::decode(&bytes).map_err(|why| format!("{}: {why}", path.display()))?;
    if profile.engine != engine.identity() {
        return Err(format!("{}: made by another engine", path.display()));
    }
    let Some(index) = profile
        .libraries
        .iter()
        .position(|key| key.name == library.name())
    else {
        return Ok(0);
    };
    if Key::of(library.path(), library.name())? != profile.libraries[index] {
        return Err(format!(
            "{}: {} has changed since it was made",
            path.display(),
            library.name().escape_debug()
        ));
    }
    let bias = library.bias();
    let code = library.code_ranges();
    let locations: Vec<u64> = profile
        .entries
        .iter()
        .filter(|entry| usize::from(entry.library) == index)
        .filter(|entry| {
            let offset = u64::from(entry.offset);
            code.iter()
                .any(|&(start, end)| (start..end).contains(&offset))
        })
        .map(|entry| {
            (bias.wrapping_add(u64::from(entry.offset)) & PC_MASK)
                | (u64::from(entry.flags) << PC_BITS)
        })
        .collect();
    // Split by turns between the threads, so each goes through the profile in the order the
    // blocks were first needed.
    let threads = pretranslate_threads().min(locations.len().max(1));
    let parts: Vec<Vec<u64>> = (0..threads)
        .map(|part| {
            locations
                .iter()
                .copied()
                .skip(part)
                .step_by(threads)
                .collect()
        })
        .collect();
    let done = std::thread::scope(|scope| {
        let workers: Vec<_> = parts
            .iter()
            .map(|part| {
                // SAFETY: each location is inside one of the library's executable segments, which
                // stay mapped and readable for the life of the process.
                scope.spawn(move || unsafe { engine.pretranslate(part) })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap_or(0))
            .sum::<usize>()
    });
    if done > 0 {
        // Translating left freed memory in these threads' malloc arenas; give it back.
        // SAFETY: malloc_trim only releases free memory.
        unsafe { libc::malloc_trim(0) };
    }
    Ok(done)
}

/// Threads translating ahead at once: `UXDROID_PRETRANSLATE_THREADS`, 1 to 8, or 1. One measured
/// 80 ms slower to RustDesk's first frame than two, and 29 MiB smaller.
fn pretranslate_threads() -> usize {
    std::env::var("UXDROID_PRETRANSLATE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| (1..=8).contains(threads))
        .unwrap_or(1)
}

/// Called by the loader for each guest library it has mapped and relocated, before its
/// initializers run.
pub(crate) fn library_loaded(library: &'static Library) {
    if std::env::var_os("UXDROID_TRANSLATION_PROFILE").is_some_and(|value| value == "0") {
        return;
    }
    let Some(directory) = directory_of(library) else {
        return;
    };
    if !directory.join(PLAN_FILE).is_file() {
        return;
    }
    if let Some(queue) = worker() {
        let _ = queue.send(library);
    }
    start_saving(directory);
}

/// The thread that translates libraries' profiles as they load, one library after another.
fn worker() -> Option<Sender<&'static Library>> {
    static QUEUE: OnceLock<Option<Mutex<Sender<&'static Library>>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = channel::<&'static Library>();
        std::thread::Builder::new()
            .name("uxdroid-pretranslate".to_string())
            .spawn(move || {
                for library in receiver {
                    let exists = directory_of(library).is_some_and(|dir| dir.join(FILE).is_file());
                    if !exists {
                        continue;
                    }
                    if let Err(why) = pretranslate(library) {
                        crate::output::write_stderr(format_args!(
                            "uxdroid: the translation profile was not used: {why}\n"
                        ));
                    }
                }
            })
            .ok()
            .map(|_| Mutex::new(sender))
    });
    let guard = queue
        .as_ref()?
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(guard.clone())
}

/// One saving thread per directory, for the life of the process.
fn start_saving(directory: PathBuf) {
    static SAVING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let saving = SAVING.get_or_init(|| Mutex::new(HashSet::new()));
    let mut started = saving
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !started.insert(directory.clone()) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("uxdroid-profile".to_string())
        .spawn(move || {
            let begun = std::time::Instant::now();
            let mut saved = 0;
            for after in SAVE_AFTER {
                std::thread::sleep(after.saturating_sub(begun.elapsed()));
                let translated = super::engine().map_or(0, |engine| engine.translated().len());
                if translated <= saved {
                    continue;
                }
                match save(&directory) {
                    Ok(_) => saved = translated,
                    Err(why) => {
                        crate::output::write_stderr(format_args!(
                            "uxdroid: cannot save the translation profile: {why}\n"
                        ));
                        return;
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Profile {
        Profile {
            engine: 0x1234,
            libraries: vec![
                Key {
                    name: "libapp.so".to_string(),
                    size: 10,
                    modified_ns: 20,
                },
                Key {
                    name: "libflutter.so".to_string(),
                    size: 30,
                    modified_ns: 40,
                },
            ],
            entries: vec![
                Entry {
                    library: 1,
                    flags: 3,
                    offset: 0x1000,
                },
                Entry {
                    library: 0,
                    flags: 0,
                    offset: 0x24,
                },
            ],
        }
    }

    #[test]
    fn a_profile_reads_back_as_written() {
        let profile = sample();
        assert_eq!(Profile::decode(&profile.encode()), Ok(profile));
    }

    #[test]
    fn a_damaged_profile_is_refused_without_reading_past_it() {
        let bytes = sample().encode();
        for length in 0..bytes.len() {
            assert!(Profile::decode(&bytes[..length]).is_err(), "{length} bytes");
        }
        let mut bad_library = bytes.clone();
        let last = bad_library.len() - 8;
        bad_library[last] = 7;
        assert_eq!(
            Profile::decode(&bad_library),
            Err("a block of a library it does not name".to_string())
        );
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&0u64.to_le_bytes());
        huge.extend_from_slice(&0u32.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Profile::decode(&huge), Err(format!("{} blocks", u32::MAX)));
    }
}
