//! `uxdroid install`: the installer's part for an app the framework launcher starts. It puts the APK's
//! native libraries where `ApplicationInfo.nativeLibraryDir` points, `<data>/data/<package>/lib`, and
//! records the ABI it chose. With the framework's runtime at hand it also prepares the app's code
//! directory, `<data>/app/<package>`, as Android's installer does: the APK as `base.apk`, its DEX
//! compiled beside it, the framework's parse of its manifest, and its library of default bodies. A
//! start then only maps what is there.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use uxdroid_apk::{ExecutionPath, extract_native_libs, host_abi, inspect};

use crate::runtime::{boot_class_path, default_runtime, failure, runtime_environment};
use crate::{EXIT_FAILURE, EXIT_USAGE, INSTALL_USAGE, fail, print};

/// Measured on RustDesk from disk: verified code started 13% sooner than none and used 5 MiB less
/// private memory than `speed`, which also took 5 times as long to compile. See benches/framework.
const COMPILER_FILTER: &str = "verify";
const INSTALLER_CLASS: &str = "dev.uxdroid.system.Installer";
/// How long the framework's installer or dex2oat may run while the package lock is held: far beyond
/// what the largest app measured needs (RustDesk compiles in under a second), so only a hung program
/// is stopped, and other installs of the package are not kept waiting forever.
const STEP_LIMIT: Duration = Duration::from_secs(600);

/// Runs `command` as `Command::output` does, but stops it after `limit`.
fn output_within(command: &mut Command, what: &str, limit: Duration) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {what}: {error}"))?;
    // Both pipes are drained while the program runs, so a full one never stalls it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: stdout.join().unwrap_or_default(),
                    stderr: stderr.join().unwrap_or_default(),
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what} did not finish within {} s and was stopped",
                    limit.as_secs()
                ));
            }
            Err(error) => return Err(format!("cannot wait for {what}: {error}")),
        }
    }
}

/// App ids become directory names, so only their documented characters are accepted.
pub(crate) fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('.')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

fn parse(args: &[OsString]) -> Option<(&Path, &Path)> {
    match args {
        [apk, flag, data] if flag == "--data" && !apk.as_encoded_bytes().starts_with(b"--") => {
            Some((Path::new(apk), Path::new(data)))
        }
        _ => None,
    }
}

pub fn run_install(args: &[OsString]) -> ExitCode {
    let Some((apk, data)) = parse(args) else {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    };
    match install(apk, data) {
        Ok((report, note)) => {
            if let Some(note) = note {
                let _ = fail(&format!("note: {note}"), 0);
            }
            print(&report)
        }
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

fn cannot(path: &Path, error: std::io::Error) -> String {
    format!("cannot write {}: {error}", path.display())
}

/// Installs of one package take turns, whichever process and PID namespace they run in: the lock is
/// on a file of the data directory, released when the returned file closes.
fn lock_package(data: &Path, package: &str) -> Result<std::fs::File, String> {
    let locks = data.join("app");
    std::fs::create_dir_all(&locks).map_err(|error| cannot(&locks, error))?;
    let path = locks.join(format!(".{package}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| cannot(&path, error))?;
    file.lock()
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    Ok(file)
}

/// What an install that died left in `directory`. Only installs hold the package lock, so none runs.
fn remove_leftovers(directory: &Path, prefixes: &[String]) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            let path = entry.path();
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
    }
}

/// The renames that put an install in place, undone in reverse if one fails, so a failed install
/// changes nothing.
#[derive(Default)]
struct Renames(Vec<(PathBuf, PathBuf)>);

impl Renames {
    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        std::fs::rename(from, to).map_err(|error| cannot(to, error))?;
        self.0.push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }

    fn undo(self) {
        for (from, to) in self.0.into_iter().rev() {
            let _ = std::fs::rename(to, from);
        }
    }
}

/// Where the framework's runtime was looked for.
enum Framework {
    Found(PathBuf),
    /// Nothing there: the app is installed unprepared.
    Missing(PathBuf),
    /// Where to look could not be told, such as without `HOME`.
    Unknown(String),
}

/// The framework's runtime, if it has one: a missing framework only means nothing is prepared.
fn framework_runtime() -> Framework {
    match default_runtime() {
        Ok(runtime) if runtime.join("framework/framework.jar").is_file() => {
            Framework::Found(runtime)
        }
        Ok(runtime) => Framework::Missing(runtime),
        Err(error) => Framework::Unknown(error),
    }
}

/// Prepares the code directory `staged`, which becomes `code`: the APK, the record and default bodies
/// the framework's installer writes, and the DEX compiled. Returns the launcher Activity and the number
/// of default bodies.
fn prepare(
    runtime: &Path,
    apk: &Path,
    staged: &Path,
    code: &Path,
    data: &Path,
) -> Result<(String, u64), String> {
    let base = staged.join("base.apk");
    std::fs::create_dir_all(staged.join("oat/x86_64")).map_err(|error| cannot(staged, error))?;
    // A read-only APK must not make its copy impossible to replace, so contents are copied, not mode.
    let mut source = std::fs::File::open(apk)
        .map_err(|error| format!("cannot read APK {}: {error}", apk.display()))?;
    let mut copy = std::fs::File::create(&base).map_err(|error| cannot(&base, error))?;
    std::io::copy(&mut source, &mut copy).map_err(|error| cannot(&base, error))?;
    drop(copy);

    std::fs::create_dir_all(data.join("dalvik-cache/x86_64"))
        .map_err(|error| cannot(data, error))?;
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let native_dir = exe.parent().ok_or("cannot locate this program")?;
    let library_path = format!(
        "{}:{}",
        runtime.join("lib64").display(),
        native_dir.display()
    );
    let boot = boot_class_path(runtime);
    let framework = format!(
        "{}:{}",
        runtime.join("framework/framework.jar").display(),
        runtime.join("framework/uxdroid-system.jar").display()
    );

    let mut installer = Command::new(runtime.join("bin/dalvikvm64"));
    installer
        .arg(format!("-Xbootclasspath:{boot}"))
        .arg(format!("-Xbootclasspath-locations:{boot}"))
        .arg(format!(
            "-Ximage:{}",
            runtime.join("framework/boot.art").display()
        ))
        .args(["-Xnorelocate", "-cp", &framework, INSTALLER_CLASS])
        .arg(runtime)
        .arg(native_dir.join("libuxdroid_system.so"))
        .arg(&base)
        .arg(code.join("base.apk"));
    runtime_environment(&mut installer, runtime, data, &library_path);
    let output = output_within(&mut installer, "the runtime", STEP_LIMIT)?;
    if !output.status.success() {
        return Err(format!(
            "the framework installer failed: {}",
            failure(&output.stderr, &["Exception"])
        ));
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        printed
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .map(str::to_string)
            .ok_or(format!("the framework installer printed no {key}"))
    };
    let launcher = value("launcher")?;
    let defaults = value("defaults")?
        .parse()
        .map_err(|_| "the framework installer printed a bad count".to_string())?;

    // The app's class loader has the framework's as its parent, so the code is compiled against it.
    let mut dex2oat = Command::new(runtime.join("bin/dex2oat64"));
    dex2oat
        .arg(format!("--compiler-filter={COMPILER_FILTER}"))
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
        .arg(format!("--class-loader-context=PCL[];PCL[{framework}]"))
        .arg(format!("--dex-file={}", base.display()))
        .arg(format!(
            "--dex-location={}",
            code.join("base.apk").display()
        ))
        .arg(format!(
            "--oat-file={}",
            staged.join("oat/x86_64/base.odex").display()
        ));
    runtime_environment(&mut dex2oat, runtime, data, &library_path);
    let output = output_within(&mut dex2oat, "dex2oat", STEP_LIMIT)?;
    if !output.status.success() {
        return Err(format!("dex2oat failed: {}", failure(&output.stderr, &[])));
    }
    Ok((launcher, defaults))
}

type Report = (String, Option<String>);

/// The guest ABI an APK's code can run as on this host, through the translator.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn translated_abi(abis: &[String]) -> Option<&'static str> {
    use crate::translation::GUEST_ABI;
    abis.iter().any(|abi| abi == GUEST_ABI).then_some(GUEST_ABI)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn translated_abi(_abis: &[String]) -> Option<&'static str> {
    None
}

/// Resolves the staged guest libraries' imports and writes the plan beside them; the report's
/// line names where the plan ends up.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn plan_translation(staging: &Path, package: &str) -> Result<String, String> {
    let summary = crate::translation::prepare(staging, package)?;
    let installed = staging
        .parent()
        .map_or_else(|| staging.to_path_buf(), |app| app.join("lib"))
        .join(crate::translation::PLAN_FILE);
    Ok(summary.describe(&installed))
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn plan_translation(_staging: &Path, _package: &str) -> Result<String, String> {
    Err("translation is not built for this host".to_string())
}

fn install(apk_path: &Path, data: &Path) -> Result<Report, String> {
    if !data.is_absolute() {
        return Err(format!(
            "the data directory must be absolute, not {}",
            data.display()
        ));
    }
    let apk = inspect(apk_path).map_err(|error| error.to_string())?;
    let package = &apk.manifest.package;
    if !valid_package(package) {
        return Err(format!("bad package name {}", package.escape_debug()));
    }
    let host = host_abi();
    let (abi, translated) = match apk.execution_path(host) {
        ExecutionPath::NeedsAbiBridge(abis) => match translated_abi(&abis) {
            Some(guest) => (guest, true),
            None => {
                return Err(format!(
                    "this APK has native code only for {}, and running it on {host} needs translation, which is not built yet",
                    abis.join(", ").escape_debug()
                ));
            }
        },
        _ => (host, false),
    };

    let _lock = lock_package(data, package)?;
    let pid = std::process::id();
    let app_dir = data.join("data").join(package);
    let libraries = app_dir.join("lib");
    let apps = data.join("app");
    let code = apps.join(package);
    remove_leftovers(
        &app_dir,
        &[".lib-".into(), ".abi-".into(), ".old-lib-".into()],
    );
    remove_leftovers(
        &apps,
        &[format!(".{package}.new-"), format!(".{package}.old-")],
    );

    // Staged beside their final places and renamed into place.
    let staging = app_dir.join(format!(".lib-{pid}"));
    let staged_code = apps.join(format!(".{package}.new-{pid}"));
    let pending = app_dir.join(format!(".abi-{pid}"));
    let old_libraries = app_dir.join(format!(".old-lib-{pid}"));
    let old_code = apps.join(format!(".{package}.old-{pid}"));
    let record = app_dir.join("abi");
    let runtime = framework_runtime();

    let result = (|| {
        std::fs::create_dir_all(&staging).map_err(|error| cannot(&staging, error))?;
        let count =
            extract_native_libs(apk_path, abi, &staging).map_err(|error| error.to_string())?;
        let translation = match translated {
            true => Some(plan_translation(&staging, package)?),
            false => None,
        };
        let prepared = match &runtime {
            Framework::Found(runtime) => Some(
                prepare(runtime, apk_path, &staged_code, &code, data).map_err(|error| {
                    format!(
                        "cannot prepare {} for the framework: {error}",
                        package.escape_debug()
                    )
                })?,
            ),
            Framework::Missing(_) | Framework::Unknown(_) => None,
        };
        std::fs::write(&pending, format!("{abi}\n")).map_err(|error| cannot(&pending, error))?;

        let mut renames = Renames::default();
        let swap = (|| {
            if libraries.exists() {
                renames.rename(&libraries, &old_libraries)?;
            }
            renames.rename(&staging, &libraries)?;
            // Without a framework, an older preparation would run old code beside new libraries.
            if code.exists() {
                renames.rename(&code, &old_code)?;
            }
            if prepared.is_some() {
                renames.rename(&staged_code, &code)?;
            }
            renames.rename(&pending, &record)
        })();
        if let Err(error) = swap {
            renames.undo();
            return Err(error);
        }
        Ok((count, prepared, translation))
    })();
    let _ = std::fs::remove_file(&pending);
    for leftover in [&staging, &staged_code, &old_libraries, &old_code] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    let (count, prepared, translation) = result?;

    let mut report = format!(
        "installed {}: {count} native libraries for {abi} in {}\n",
        package.escape_debug(),
        libraries.display()
    );
    if let Some(translation) = translation {
        report.push_str(&translation);
    }
    let note = match (prepared, runtime) {
        (Some((launcher, defaults)), _) => {
            report.push_str(&format!(
                "prepared {} in {}: code compiled ({COMPILER_FILTER}), manifest recorded with launcher {}, {defaults} default bodies\n",
                package.escape_debug(),
                code.display(),
                launcher.escape_debug()
            ));
            None
        }
        (None, Framework::Missing(runtime)) => Some(format!(
            "no framework in {}, so {} is not prepared: its manifest is parsed, its default bodies made and its code run uncompiled at each start",
            runtime.display().to_string().escape_debug(),
            package.escape_debug()
        )),
        (None, Framework::Unknown(error)) => Some(format!(
            "cannot look for the framework ({}), so {} is not prepared: its manifest is parsed, its default bodies made and its code run uncompiled at each start",
            error.escape_debug(),
            package.escape_debug()
        )),
        (None, Framework::Found(_)) => None,
    };
    Ok((report, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_that_hangs_is_stopped_at_its_limit() {
        let started = Instant::now();
        let mut hung = Command::new("sleep");
        hung.arg("30");
        let error = output_within(&mut hung, "sleep", Duration::from_secs(1)).unwrap_err();
        assert_eq!(error, "sleep did not finish within 1 s and was stopped");
        assert!(started.elapsed() < Duration::from_secs(5));
        // A step that finishes in time gives what `output` gives.
        let mut quick = Command::new("sh");
        quick.args(["-c", "echo out; echo err >&2; exit 3"]);
        let output = output_within(&mut quick, "sh", Duration::from_secs(5)).unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(
            (&output.stdout[..], &output.stderr[..]),
            (&b"out\n"[..], &b"err\n"[..])
        );
    }
}
