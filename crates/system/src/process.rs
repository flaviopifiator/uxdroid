//! Natives of `android.os.Process` that read the kernel's view of this process.

use jni_sys::{JNIEnv, jclass, jlong, jlongArray, jobjectArray, jsize, jstring};

use crate::{Method, java_string, register};

/// For each field, the number after it on the first line that starts with it, as Android's
/// `Process.readProcLines` reads `/proc/<pid>/status`: "VmRSS:\t  1234 kB" gives 1234.
pub fn proc_lines(text: &str, fields: &[String]) -> Vec<Option<i64>> {
    fields
        .iter()
        .map(|field| {
            let rest = text
                .lines()
                .find_map(|line| line.strip_prefix(field.as_str()))?;
            let rest = rest.trim_start_matches([' ', '\t']);
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest[..end].parse().ok()
        })
        .collect()
}

/// `readProcLines(String path, String[] fields, long[] out)`: fields not found keep their value.
unsafe extern "system" fn read_proc_lines(
    env: *mut JNIEnv,
    _class: jclass,
    path: jstring,
    fields: jobjectArray,
    out: jlongArray,
) {
    if fields.is_null() || out.is_null() {
        return;
    }
    // SAFETY: ART passes a valid environment, a string and two arrays.
    unsafe {
        let jni = &**env;
        let Some(path) = java_string(env, path) else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let count = (jni.v1_1.GetArrayLength)(env, fields).min((jni.v1_1.GetArrayLength)(env, out));
        let names: Vec<String> = (0..count)
            .map(|i| {
                let name = (jni.v1_1.GetObjectArrayElement)(env, fields, i);
                let owned = java_string(env, name as jstring).unwrap_or_default();
                crate::java::delete_local(env, name);
                owned
            })
            .collect();
        for (i, value) in proc_lines(&text, &names).into_iter().enumerate() {
            if let Some(value) = value {
                let value: jlong = value;
                (jni.v1_1.SetLongArrayRegion)(env, out, i as jsize, 1, &value);
            }
        }
    }
}

/// Writes `name` over this process' command line, as Android's zygote does when it names an app's
/// process, so `/proc/self/cmdline` and tools that read it give the process name. The arguments
/// the process was started with are the space available; a longer name is cut to fit.
pub fn set_command_line(name: &str) -> std::io::Result<()> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "an unexpected /proc/self/stat",
        )
    };
    // Fields after the command, which may hold spaces and ends at the last ')': state is field 3,
    // arg_start 48 and arg_end 49.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .ok_or_else(invalid)?
        .1
        .split_whitespace()
        .collect();
    let field = |n: usize| -> std::io::Result<usize> {
        fields
            .get(n - 3)
            .and_then(|value| value.parse().ok())
            .ok_or_else(invalid)
    };
    let (start, end) = (field(48)?, field(49)?);
    if end <= start {
        return Err(invalid());
    }
    let name = &name.as_bytes()[..name.len().min(end - start - 1)];
    // SAFETY: the kernel reports the argument block of this process, which lives on the main
    // thread's stack for the life of the process and is writable; it is overwritten in place.
    unsafe {
        let block = std::slice::from_raw_parts_mut(start as *mut u8, end - start);
        block.fill(0);
        block[..name.len()].copy_from_slice(name);
    }
    let mut comm = [0u8; 16];
    for (to, from) in comm.iter_mut().zip(name.iter().take(15)) {
        *to = *from;
    }
    // SAFETY: PR_SET_NAME reads a terminated string of at most 16 bytes.
    unsafe { libc::prctl(libc::PR_SET_NAME, comm.as_ptr()) };
    Ok(())
}

/// `setArgV0Native(String name)`, which `ActivityThread` calls with the process name.
unsafe extern "system" fn set_arg_v0(env: *mut JNIEnv, _class: jclass, name: jstring) {
    // SAFETY: ART passes a valid environment and a string or null.
    let Some(name) = (unsafe { java_string(env, name) }) else {
        return;
    };
    if let Err(error) = set_command_line(&name) {
        eprintln!("uxdroid: cannot name the process {name}: {error}");
    }
}

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 2] = [
        (
            c"readProcLines",
            c"(Ljava/lang/String;[Ljava/lang/String;[J)V",
            f!(read_proc_lines),
        ),
        (c"setArgV0Native", c"(Ljava/lang/String;)V", f!(set_arg_v0)),
    ];
    // SAFETY: the table matches the framework's declaration in Android 16.
    unsafe { register(env, c"android/os/Process", &methods) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_after_fields_and_skips_missing_ones() {
        let status =
            "Name:\tapp\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  51234 kB\nRssAnon:\t7 kB\n";
        let fields = ["Uid:", "VmRSS:", "VmSwap:", "RssAnon:", "Name:"].map(String::from);
        assert_eq!(
            proc_lines(status, &fields),
            [Some(1000), Some(51234), None, Some(7), None]
        );
    }
}
