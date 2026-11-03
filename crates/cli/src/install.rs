//! `uxdroid install`: the installer's part for an app the framework launcher starts. It puts the APK's
//! native libraries where `ApplicationInfo.nativeLibraryDir` points, `<data>/data/<package>/lib`, and
//! records the ABI it chose. With the framework's runtime at hand it also prepares the app's code
//! directory, `<data>/app/<package>`, as Android's installer does: the APK as `base.apk`, its DEX
//! compiled beside it, the framework's parse of its manifest, and its library of default bodies. A
//! start then only maps what is there.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use uxdroid_apk::{ExecutionPath, extract_native_libs, host_abi, inspect};

use crate::runtime::{boot_class_path, default_runtime, failure, runtime_environment};
use crate::{EXIT_FAILURE, EXIT_USAGE, INSTALL_USAGE, fail, print};

/// Measured on RustDesk from disk: verified code started 13% sooner than none and used 5 MiB less
/// private memory than `speed`, which also took 5 times as long to compile. See benches/framework.
const COMPILER_FILTER: &str = "verify";
const INSTALLER_CLASS: &str = "dev.uxdroid.system.Installer";

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

/// The framework's runtime, if it has one: a missing framework only means nothing is prepared.
fn framework_runtime() -> Result<PathBuf, String> {
    let runtime = default_runtime()?;
    if runtime.join("framework/framework.jar").is_file() {
        Ok(runtime)
    } else {
        Err(runtime.display().to_string())
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
    let output = installer
        .output()
        .map_err(|error| format!("cannot start the runtime: {error}"))?;
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
    let output = dex2oat
        .output()
        .map_err(|error| format!("cannot start dex2oat: {error}"))?;
    if !output.status.success() {
        return Err(format!("dex2oat failed: {}", failure(&output.stderr, &[])));
    }
    Ok((launcher, defaults))
}

type Report = (String, Option<String>);

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
    let abi = host_abi();
    if let ExecutionPath::NeedsAbiBridge(abis) = apk.execution_path(abi) {
        return Err(format!(
            "this APK has native code only for {}, and running it on {abi} needs translation, which is not built yet",
            abis.join(", ").escape_debug()
        ));
    }

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
        let prepared = match &runtime {
            Ok(runtime) => Some(
                prepare(runtime, apk_path, &staged_code, &code, data).map_err(|error| {
                    format!(
                        "cannot prepare {} for the framework: {error}",
                        package.escape_debug()
                    )
                })?,
            ),
            Err(_) => None,
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
        Ok((count, prepared))
    })();
    let _ = std::fs::remove_file(&pending);
    for leftover in [&staging, &staged_code, &old_libraries, &old_code] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    let (count, prepared) = result?;

    let mut report = format!(
        "installed {}: {count} native libraries for {abi} in {}\n",
        package.escape_debug(),
        libraries.display()
    );
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
        (None, Err(runtime)) => Some(format!(
            "no framework in {}, so {} is not prepared: its manifest is parsed, its default bodies made and its code run uncompiled at each start",
            runtime.escape_debug(),
            package.escape_debug()
        )),
        (None, Ok(_)) => None,
    };
    Ok((report, note))
}
