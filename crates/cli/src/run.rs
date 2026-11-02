//! `uxdroid run`: starts an APK's launcher Activity as this process.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use uxdroid_apk::{ExecutionPath, extract_native_libs, host_abi, inspect};

use crate::{EXIT_FAILURE, EXIT_USAGE, RUN_USAGE, fail};

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
const LAUNCHER_CLASS: &str = "dev.uxdroid.runtime.Launcher";

struct RunArgs<'a> {
    apk: &'a Path,
    runtime: Option<PathBuf>,
    exit_after_first_frame: bool,
}

fn parse(args: &[OsString]) -> Option<RunArgs<'_>> {
    let (apk, mut rest) = args.split_first()?;
    if apk.as_encoded_bytes().starts_with(b"--") {
        return None;
    }
    let mut parsed = RunArgs {
        apk: Path::new(apk),
        runtime: None,
        exit_after_first_frame: false,
    };
    while let Some((flag, remaining)) = rest.split_first() {
        if flag == "--exit-after-first-frame" && !parsed.exit_after_first_frame {
            parsed.exit_after_first_frame = true;
            rest = remaining;
        } else if flag == "--runtime" && parsed.runtime.is_none() {
            let (value, remaining) = remaining.split_first()?;
            parsed.runtime = Some(PathBuf::from(value));
            rest = remaining;
        } else {
            return None;
        }
    }
    Some(parsed)
}

fn home_dir(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(variable) {
        return Ok(PathBuf::from(value));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(fallback))
}

fn boot_class_path(runtime: &Path) -> String {
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

/// App ids become directory names, so only their documented characters are accepted.
fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('.')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

fn runtime_environment(command: &mut Command, runtime: &Path, data: &Path, library_path: &str) {
    command
        .env("ANDROID_ROOT", runtime)
        .env("ANDROID_ART_ROOT", runtime)
        .env("ANDROID_I18N_ROOT", runtime.join("com.android.i18n"))
        .env("ANDROID_TZDATA_ROOT", runtime.join("com.android.tzdata"))
        .env("ANDROID_DATA", data)
        .env("LD_LIBRARY_PATH", library_path);
}

/// Compiles the APK to machine code once, until the APK changes.
fn compile_app(
    runtime: &Path,
    app: &Path,
    odex: &Path,
    data: &Path,
    library_path: &str,
) -> Result<(), String> {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|meta| meta.modified());
    if let (Ok(compiled), Ok(source)) = (modified(odex), modified(app))
        && compiled >= source
    {
        return Ok(());
    }
    let boot = boot_class_path(runtime);
    let mut command = Command::new(runtime.join("bin/dex2oat64"));
    command
        .arg("--compiler-filter=speed")
        .arg("--instruction-set=x86_64")
        .arg("--host")
        .arg(format!("--android-root={}", runtime.display()))
        .args(["--runtime-arg", &format!("-Xbootclasspath:{boot}")])
        .args([
            "--runtime-arg",
            &format!("-Xbootclasspath-locations:{boot}"),
        ])
        .arg(format!(
            "--boot-image={}",
            runtime.join("framework/boot.art").display()
        ))
        .arg(format!(
            "--class-loader-context=PCL[{}]",
            runtime.join("platform/platform.jar").display()
        ))
        .arg(format!("--dex-file={}", app.display()))
        .arg(format!("--oat-file={}", odex.display()));
    runtime_environment(&mut command, runtime, data, library_path);
    let output = command
        .output()
        .map_err(|error| format!("cannot start dex2oat: {error}"))?;
    if !output.status.success() {
        let log = String::from_utf8_lossy(&output.stderr);
        let last = log.lines().last().unwrap_or("").escape_debug().to_string();
        return Err(format!("dex2oat failed: {last}"));
    }
    Ok(())
}

fn prepare_and_exec(args: &RunArgs) -> Result<std::convert::Infallible, String> {
    let apk = inspect(args.apk).map_err(|error| error.to_string())?;
    let abi = host_abi();
    if let ExecutionPath::NeedsAbiBridge(abis) = apk.execution_path(abi) {
        return Err(format!(
            "this APK has native code only for {}, and running it on {abi} needs translation, which is not built yet",
            abis.join(", ").escape_debug()
        ));
    }
    let activity = apk
        .manifest
        .launcher
        .as_deref()
        .ok_or("this APK has no launcher Activity")?;
    if !valid_package(&apk.manifest.package) {
        return Err(format!(
            "bad package name {}",
            apk.manifest.package.escape_debug()
        ));
    }

    let runtime = match &args.runtime {
        Some(runtime) => runtime.clone(),
        None => match std::env::var_os("UXDROID_RUNTIME") {
            Some(runtime) => PathBuf::from(runtime),
            None => home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid/runtime"),
        },
    };
    let runtime = std::fs::canonicalize(&runtime)
        .map_err(|error| format!("cannot find the runtime at {}: {error}", runtime.display()))?;

    let app_dir = home_dir("XDG_CACHE_HOME", ".cache")?
        .join("uxdroid")
        .join(&apk.manifest.package);
    let oat_dir = app_dir.join("oat/x86_64");
    let lib_dir = app_dir.join("lib");
    let data = app_dir.join("data");
    let cache_error =
        |error: std::io::Error| format!("cannot prepare {}: {error}", app_dir.display());
    std::fs::create_dir_all(&oat_dir).map_err(cache_error)?;
    std::fs::create_dir_all(data.join("dalvik-cache/x86_64")).map_err(cache_error)?;
    std::fs::create_dir_all(&lib_dir).map_err(cache_error)?;

    let app = app_dir.join("app.apk");
    let source_modified = std::fs::metadata(args.apk).and_then(|meta| meta.modified());
    let copied_modified = std::fs::metadata(&app).and_then(|meta| meta.modified());
    let stale = match (source_modified, copied_modified) {
        (Ok(source), Ok(copied)) => source > copied,
        _ => true,
    };
    if stale {
        std::fs::copy(args.apk, &app).map_err(cache_error)?;
        extract_native_libs(args.apk, abi, &lib_dir).map_err(|error| error.to_string())?;
    }

    // The uxdroid native libraries are installed next to this program.
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let native_dir = exe.parent().ok_or("cannot locate this program")?;
    let library_path = format!(
        "{}:{}",
        runtime.join("lib64").display(),
        native_dir.display()
    );
    compile_app(
        &runtime,
        &app,
        &oat_dir.join("app.odex"),
        &data,
        &library_path,
    )?;

    let platform = runtime.join("platform/platform.jar");
    let boot = boot_class_path(&runtime);
    let mut command = Command::new(runtime.join("bin/dalvikvm64"));
    command
        .arg(format!("-Xbootclasspath:{boot}"))
        .arg(format!("-Xbootclasspath-locations:{boot}"))
        .arg(format!(
            "-Ximage:{}",
            runtime.join("framework/boot.art").display()
        ))
        // These keep memory low; see runtime/art/run.sh for what each one saves.
        .args(["-Xnorelocate", "-XX:HeapMinFree=256k", "-XX:HeapMaxFree=1m"])
        .args([
            "-XX:NativeBridge=libuxdroid_nativebridge.so",
            "-Xforce-nb-testing",
        ])
        .arg(format!(
            "-Djava.library.path={}:{}",
            lib_dir.display(),
            native_dir.display()
        ))
        .arg(format!("-Duxdroid.package={}", apk.manifest.package))
        .arg(format!(
            "-Duxdroid.exit_after_first_frame={}",
            args.exit_after_first_frame
        ))
        .args(["-cp", &format!("{}:{}", platform.display(), app.display())])
        .args([LAUNCHER_CLASS, activity]);
    runtime_environment(&mut command, &runtime, &data, &library_path);
    Err(format!("cannot start the runtime: {}", command.exec()))
}

pub fn run_app(args: &[OsString]) -> ExitCode {
    let Some(args) = parse(args) else {
        return fail(RUN_USAGE, EXIT_USAGE);
    };
    match prepare_and_exec(&args) {
        Ok(never) => match never {},
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}
