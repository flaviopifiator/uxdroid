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

/// # Safety
/// `env` must be the calling thread's JNI environment.
pub unsafe fn register_natives(env: *mut JNIEnv) -> Result<(), String> {
    let methods: [Method; 1] = [(
        c"readProcLines",
        c"(Ljava/lang/String;[Ljava/lang/String;[J)V",
        f!(read_proc_lines),
    )];
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
