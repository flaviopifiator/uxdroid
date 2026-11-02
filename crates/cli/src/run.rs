//! `uxdroid run`: starts an APK's launcher Activity as this process.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use uxdroid_apk::{ExecutionPath, extract_native_libs, fingerprint, host_abi, inspect};

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
    // The XDG specification treats an empty value as unset.
    if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
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

/// Compiles the APK at `work` to machine code, as the file it will become at `location`.
fn compile_app(
    runtime: &Path,
    work: &Path,
    location: &Path,
    data: &Path,
    library_path: &str,
) -> Result<(), String> {
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
        .arg(format!("--dex-file={}", work.join("app.apk").display()))
        .arg(format!(
            "--dex-location={}",
            location.join("app.apk").display()
        ))
        .arg(format!(
            "--oat-file={}",
            work.join("oat/x86_64/app.odex").display()
        ));
    runtime_environment(&mut command, runtime, data, library_path);
    let output = command
        .output()
        .map_err(|error| format!("cannot start dex2oat: {error}"))?;
    if !output.status.success() {
        // Its last line is usually a timing summary, so errors are searched for first.
        let log = String::from_utf8_lossy(&output.stderr);
        let line = log
            .lines()
            .rfind(|line| line.contains(" E ") || line.contains(" F "))
            .or(log.lines().last())
            .unwrap_or("");
        return Err(format!("dex2oat failed: {}", line.escape_debug()));
    }
    Ok(())
}

/// The identity of an installed app: its contents and the runtime it was compiled against, whose
/// rebuild would make ART quietly discard the compiled code.
fn install_id(apk: &Path, runtime: &Path) -> Result<String, String> {
    let contents = fingerprint(apk).map_err(|error| error.to_string())?;
    let image = std::fs::metadata(runtime.join("framework/x86_64/boot.oat"))
        .map_err(|error| format!("cannot read the runtime: {error}"))?;
    let built = image.mtime() as u64 ^ image.len().rotate_left(32);
    Ok(format!("{:016x}", contents ^ built.rotate_left(17)))
}

/// Unpacks and compiles the APK into a private directory, then renames it into place, so a failed
/// or concurrent install never leaves anything another run could pick up.
fn install(
    args: &RunArgs,
    runtime: &Path,
    package_dir: &Path,
    id: &str,
    data: &Path,
    library_path: &str,
) -> Result<PathBuf, String> {
    let installed = package_dir.join(id);
    if installed.is_dir() {
        return Ok(installed);
    }
    let work = package_dir.join(format!(".installing-{}", std::process::id()));
    let result = (|| {
        let cannot =
            |error: std::io::Error| format!("cannot install in {}: {error}", work.display());
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(work.join("oat/x86_64")).map_err(cannot)?;
        std::fs::create_dir_all(work.join("lib")).map_err(cannot)?;
        // A read-only APK must not make its copy impossible to replace, so contents are copied
        // and not its mode.
        let mut source = std::fs::File::open(args.apk).map_err(cannot)?;
        let mut copy = std::fs::File::create(work.join("app.apk")).map_err(cannot)?;
        std::io::copy(&mut source, &mut copy).map_err(cannot)?;
        extract_native_libs(args.apk, host_abi(), &work.join("lib"))
            .map_err(|error| error.to_string())?;
        compile_app(runtime, &work, &installed, data, library_path)?;
        match std::fs::rename(&work, &installed) {
            Ok(()) => Ok(()),
            // Another run finished first with the same contents, so its copy serves just as well.
            Err(_) if installed.is_dir() => {
                let _ = std::fs::remove_dir_all(&work);
                Ok(())
            }
            Err(error) => Err(cannot(error)),
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&work);
    }
    result?;
    remove_older_installs(package_dir, id);
    Ok(installed)
}

/// Best effort: a leftover only costs disk, and a running app keeps its files open.
fn remove_older_installs(package_dir: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(package_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_install =
            name.len() == 16 && name.as_encoded_bytes().iter().all(u8::is_ascii_hexdigit);
        if is_install && name != current {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
        None => match std::env::var_os("UXDROID_RUNTIME").filter(|value| !value.is_empty()) {
            Some(runtime) => PathBuf::from(runtime),
            None => home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid/runtime"),
        },
    };
    let runtime = std::fs::canonicalize(&runtime)
        .map_err(|error| format!("cannot find the runtime at {}: {error}", runtime.display()))?;

    let package_dir = home_dir("XDG_CACHE_HOME", ".cache")?
        .join("uxdroid")
        .join(&apk.manifest.package);
    let data = package_dir.join("data");
    std::fs::create_dir_all(data.join("dalvik-cache/x86_64"))
        .map_err(|error| format!("cannot prepare {}: {error}", package_dir.display()))?;
    // Class paths separate entries with a colon, and have no way to escape one.
    let package_dir = std::fs::canonicalize(&package_dir).map_err(|error| error.to_string())?;
    if package_dir.as_os_str().as_encoded_bytes().contains(&b':') {
        return Err(format!(
            "the cache path {} contains a colon",
            package_dir.display()
        ));
    }
    let data = package_dir.join("data");

    // The uxdroid native libraries are installed next to this program.
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let native_dir = exe.parent().ok_or("cannot locate this program")?;
    let library_path = format!(
        "{}:{}",
        runtime.join("lib64").display(),
        native_dir.display()
    );
    let id = install_id(args.apk, &runtime)?;
    let installed = install(args, &runtime, &package_dir, &id, &data, &library_path)?;
    let app = installed.join("app.apk");
    let lib_dir = installed.join("lib");

    let platform = runtime.join("platform/platform.jar");
    let boot = boot_class_path(&runtime);
    let mut command = Command::new(runtime.join("bin/dalvikvm64"));
    command
        // Native bridge testing mode creates ./code_cache, so the current directory is the app's.
        .current_dir(&data)
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
