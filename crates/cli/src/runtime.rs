//! The packaged runtime: where it is, and how to run its programs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// ART's name for this host's instruction set, which names the boot image's, the oat files' and the
/// dalvik-cache's directories and is dex2oat's `--instruction-set`: one runtime package per CPU.
pub(crate) const INSTRUCTION_SET: &str = if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x86_64"
};

/// The boot class path, in the order the boot image was compiled with.
const BOOT_JARS: [&str; 7] = [
    "core-oj",
    "core-libart",
    "okhttp",
    "bouncycastle",
    "apache-xml",
    "core-icu4j",
    "conscrypt",
];

pub(crate) fn home_dir(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    // The XDG specification treats an empty value as unset.
    if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(fallback))
}

/// `UXDROID_RUNTIME`, then `$XDG_DATA_HOME/uxdroid/runtime`.
pub(crate) fn default_runtime() -> Result<PathBuf, String> {
    match std::env::var_os("UXDROID_RUNTIME").filter(|value| !value.is_empty()) {
        Some(runtime) => Ok(PathBuf::from(runtime)),
        None => Ok(home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid/runtime")),
    }
}

pub(crate) fn boot_class_path(runtime: &Path) -> String {
    let jars: Vec<String> = BOOT_JARS
        .iter()
        .map(|jar| {
            runtime
                .join("javalib")
                .join(format!("{jar}.jar"))
                .display()
                .to_string()
        })
        .collect();
    jars.join(":")
}

/// How the framework's programs run ART, as `runtime/art/run.sh` does: the boot image at its compiled
/// address, so its pages stay clean and shared, and heap limits that return freed memory sooner.
pub(crate) fn art_flags(runtime: &Path) -> Vec<String> {
    let boot = boot_class_path(runtime);
    vec![
        format!("-Xbootclasspath:{boot}"),
        format!("-Xbootclasspath-locations:{boot}"),
        format!("-Ximage:{}", runtime.join("framework/boot.art").display()),
        "-Xnorelocate".into(),
        "-XX:HeapMinFree=256k".into(),
        "-XX:HeapMaxFree=1m".into(),
        "-XX:LargeObjectSpace=disabled".into(),
        "-XX:ForegroundHeapGrowthMultiplier=1.0".into(),
    ]
}

pub(crate) fn runtime_environment(
    command: &mut Command,
    runtime: &Path,
    data: &Path,
    library_path: &str,
) {
    command
        .env("ANDROID_ROOT", runtime)
        .env("ANDROID_ART_ROOT", runtime)
        .env("ANDROID_I18N_ROOT", runtime.join("com.android.i18n"))
        .env("ANDROID_TZDATA_ROOT", runtime.join("com.android.tzdata"))
        .env("ANDROID_DATA", data)
        .env("LD_LIBRARY_PATH", library_path);
}

/// The line of a runtime program's log that says why it failed: the last one holding one of `first`,
/// else the last error or fatal line, else the last line. ART's last line is usually a timing summary.
pub(crate) fn failure(log: &[u8], first: &[&str]) -> String {
    let log = String::from_utf8_lossy(log);
    let line = log
        .lines()
        .rfind(|line| first.iter().any(|text| line.contains(text)))
        .or(log
            .lines()
            .rfind(|line| line.contains(" E ") || line.contains(" F ")))
        .or(log.lines().last())
        .unwrap_or("");
    line.escape_debug().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instruction_set_is_art_s_name_for_this_cpu() {
        // ART names its oat, dalvik-cache and boot image directories by instruction set.
        let expected = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            _ => "x86_64",
        };
        assert_eq!(INSTRUCTION_SET, expected);
    }
}
