//! `uxdroid install`: the installer's part for an app the framework launcher starts. It verifies the
//! signature of every APK given, and that an update is signed as the installed app is. It puts the
//! APK's native libraries where `ApplicationInfo.nativeLibraryDir` points, `<data>/data/<package>/lib`,
//! and records the ABI it chose. An app delivered as a base APK plus splits, as files or as a bundle,
//! gets the splits that fit this machine, as `split_<name>.apk` beside `base.apk` in its code
//! directory, `<data>/app/<package>`. With the framework's runtime at hand it also prepares that
//! directory as Android's installer does: the DEX compiled beside the APKs, the framework's parse of
//! the manifest, and the library of default bodies. A start then only maps what is there.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use uxdroid_apk::{
    ExecutionPath, Host, Kind, Part, Plan, Scheme, bundle_entries, extract_bundled,
    extract_native_libs, host_abi, inspect, plan_splits, verify_signature,
};

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
/// The density the framework reports, `DisplayService.DENSITY` in system/java: density splits are
/// chosen for it.
const HOST_DENSITY: u32 = 160;
/// Android 11: apps targeting it or later need a v2 or later signature.
const V2_REQUIRED_FROM_SDK: u32 = 30;

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

fn parse(args: &[OsString]) -> Option<(Vec<&Path>, &Path)> {
    match args {
        [apks @ .., flag, data]
            if flag == "--data"
                && !apks.is_empty()
                && apks
                    .iter()
                    .all(|apk| !apk.as_encoded_bytes().starts_with(b"--")) =>
        {
            Some((apks.iter().map(Path::new).collect(), Path::new(data)))
        }
        _ => None,
    }
}

pub fn run_install(args: &[OsString]) -> ExitCode {
    let Some((apks, data)) = parse(args) else {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    };
    match install(&apks, data) {
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

/// Copies `from` to `to`. A read-only APK must not make its copy impossible to replace, so contents
/// are copied, not mode.
fn copy_apk(from: &Path, to: &Path) -> Result<(), String> {
    let mut source = std::fs::File::open(from)
        .map_err(|error| format!("cannot read APK {}: {error}", from.display()))?;
    let mut copy = std::fs::File::create(to).map_err(|error| cannot(to, error))?;
    std::io::copy(&mut source, &mut copy).map_err(|error| cannot(to, error))?;
    Ok(())
}

/// Fills the code directory `staged` with the base APK as `base.apk` and each split as
/// `split_<name>.apk`, and, when there are splits, their names in `splits`, in the order Android
/// lists them.
fn stage_code(staged: &Path, parts: &[Part], plan: &Plan) -> Result<(), String> {
    std::fs::create_dir_all(staged).map_err(|error| cannot(staged, error))?;
    copy_apk(&parts[plan.base].path, &staged.join("base.apk"))?;
    let mut record = String::new();
    for (index, _) in &plan.chosen {
        let name = split_name(&parts[*index]);
        copy_apk(
            &parts[*index].path,
            &staged.join(format!("split_{name}.apk")),
        )?;
        record.push_str(name);
        record.push('\n');
    }
    if !record.is_empty() {
        let path = staged.join("splits");
        std::fs::write(&path, record).map_err(|error| cannot(&path, error))?;
    }
    Ok(())
}

fn split_name(part: &Part) -> &str {
    part.apk.manifest.split.as_deref().unwrap_or_default()
}

/// Prepares the code directory `staged`, which becomes `code` and already holds the APKs: the record
/// and default bodies the framework's installer writes, and the DEX of the base and of each split in
/// `with_code` compiled. Returns the launcher Activity and the number of default bodies.
fn prepare(
    runtime: &Path,
    staged: &Path,
    code: &Path,
    data: &Path,
    with_code: &[&str],
) -> Result<(String, u64), String> {
    let base = staged.join("base.apk");
    std::fs::create_dir_all(staged.join("oat/x86_64")).map_err(|error| cannot(staged, error))?;

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
    // A split's code joins the base's loader after the base and the splits before it, as Android
    // loads splits that are not isolated. As installd does, those are named relative to the directory
    // dex2oat reads them from, since their installed paths do not exist yet; ART matches them by name.
    let mut earlier: Vec<String> = Vec::new();
    for file in std::iter::once("base".to_string())
        .chain(with_code.iter().map(|name| format!("split_{name}")))
    {
        let location = code.join(format!("{file}.apk"));
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
            .arg(format!(
                "--class-loader-context=PCL[{}];PCL[{framework}]",
                earlier.join(":")
            ))
            .arg(format!(
                "--dex-file={}",
                staged.join(format!("{file}.apk")).display()
            ))
            .arg(format!("--dex-location={}", location.display()))
            .arg(format!(
                "--oat-file={}",
                staged.join(format!("oat/x86_64/{file}.odex")).display()
            ));
        if !earlier.is_empty() {
            dex2oat.arg(format!("--classpath-dir={}", staged.display()));
        }
        runtime_environment(&mut dex2oat, runtime, data, &library_path);
        let output = output_within(&mut dex2oat, "dex2oat", STEP_LIMIT)?;
        if !output.status.success() {
            return Err(format!("dex2oat failed: {}", failure(&output.stderr, &[])));
        }
        earlier.push(format!("{file}.apk"));
    }
    Ok((launcher, defaults))
}

/// A directory of `<data>/app` that holds APKs unpacked from bundles while this install runs. The
/// lock on its `lock` file says it is in use, so an install can tell what a dead one left.
struct Unpacked {
    directory: PathBuf,
    _lock: std::fs::File,
}

impl Unpacked {
    fn new(data: &Path) -> Result<Self, String> {
        let apps = data.join("app");
        std::fs::create_dir_all(&apps).map_err(|error| cannot(&apps, error))?;
        if let Ok(entries) = std::fs::read_dir(&apps) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !entry.file_name().to_string_lossy().starts_with(".unpack-") {
                    continue;
                }
                let free = std::fs::File::open(path.join("lock"))
                    .map(|file| file.try_lock().is_ok())
                    .unwrap_or(true);
                if free {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        // The directory takes its swept name only once its lock is held, so another install never
        // sees it unlocked.
        let name = format!("unpack-{}-{stamp}", std::process::id());
        let staging = apps.join(format!(".staging-{name}"));
        std::fs::create_dir(&staging).map_err(|error| cannot(&staging, error))?;
        let path = staging.join("lock");
        let lock = std::fs::File::create(&path).map_err(|error| cannot(&path, error))?;
        lock.lock()
            .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
        let directory = apps.join(format!(".{name}"));
        std::fs::rename(&staging, &directory).map_err(|error| cannot(&directory, error))?;
        Ok(Self {
            directory,
            _lock: lock,
        })
    }
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Reads and verifies every APK given, unpacking bundles into `unpacked`.
fn gather(
    apks: &[&Path],
    data: &Path,
    unpacked: &mut Option<Unpacked>,
) -> Result<Vec<Part>, String> {
    let read = |origin: String, path: PathBuf| -> Result<Part, String> {
        // As Android does, the signature is checked before anything of the APK is parsed.
        let signature = verify_signature(&path).map_err(|error| match error {
            uxdroid_apk::Error::Unsigned | uxdroid_apk::Error::BadSignature(_) => {
                format!("{origin}: {error}")
            }
            other => other.to_string(),
        })?;
        let apk = inspect(&path).map_err(|error| error.to_string())?;
        // Android 11 stopped accepting a JAR signature alone from apps that target it or later.
        let target = apk
            .manifest
            .target_sdk
            .as_deref()
            .and_then(|sdk| sdk.parse::<u32>().ok());
        if let Some(target) = target.filter(|target| *target >= V2_REQUIRED_FROM_SDK)
            && signature.scheme == Scheme::V1
        {
            return Err(format!(
                "{origin}: it targets SDK {target}, which needs a v2 or later signature, and it has only v1"
            ));
        }
        Ok(Part {
            origin,
            path,
            apk,
            signature,
        })
    };
    let mut parts = Vec::new();
    for apk in apks {
        let Some(entries) = bundle_entries(apk).map_err(|error| match error {
            uxdroid_apk::Error::Splits(reason) => format!("{}: {reason}", apk.display()),
            other => other.to_string(),
        })?
        else {
            parts.push(read(apk.display().to_string(), apk.to_path_buf())?);
            continue;
        };
        let directory = match unpacked {
            Some(unpacked) => unpacked.directory.clone(),
            None => unpacked.insert(Unpacked::new(data)?).directory.clone(),
        };
        for entry in entries {
            let path = directory.join(format!("{}.apk", parts.len()));
            extract_bundled(apk, &entry, &path).map_err(|error| error.to_string())?;
            parts.push(read(
                format!("{}!{}", apk.display(), entry.escape_debug()),
                path,
            )?);
        }
    }
    Ok(parts)
}

/// The languages of this user's locale, as `LANGUAGE` and the locale variables name them; English
/// when none is set.
fn host_languages() -> Vec<String> {
    let language = |locale: &str| {
        locale
            .split(['_', '.', '@', '-'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    let mut languages: Vec<String> = Vec::new();
    if let Some(locale) =
        locale.filter(|locale| locale != "C" && locale != "POSIX" && !locale.starts_with("C."))
    {
        let preferred = std::env::var("LANGUAGE").unwrap_or_default();
        for item in preferred.split(':').chain([locale.as_str()]) {
            let code = language(item);
            if !code.is_empty() && !languages.contains(&code) {
                languages.push(code);
            }
        }
    }
    if languages.is_empty() {
        languages.push("en".to_string());
    }
    languages
}

fn describe(list: &[(String, Kind)]) -> String {
    list.iter()
        .map(|(name, kind)| format!("{} ({kind})", name.escape_debug()))
        .collect::<Vec<_>>()
        .join(", ")
}

type Report = (String, Option<String>);

fn install(apks: &[&Path], data: &Path) -> Result<Report, String> {
    if !data.is_absolute() {
        return Err(format!(
            "the data directory must be absolute, not {}",
            data.display()
        ));
    }
    let mut unpacked = None;
    let parts = gather(apks, data, &mut unpacked)?;
    let abi = host_abi();
    let host = Host {
        abi: abi.to_string(),
        density: HOST_DENSITY,
        languages: host_languages(),
    };
    let plan = plan_splits(&parts, &host).map_err(|error| error.to_string())?;
    let base = &parts[plan.base];
    let package = &base.apk.manifest.package;
    if !valid_package(package) {
        return Err(format!("bad package name {}", package.escape_debug()));
    }
    let abi_split = plan
        .chosen
        .iter()
        .any(|(_, kind)| matches!(kind, Kind::Abi(_)));
    if !abi_split && let ExecutionPath::NeedsAbiBridge(abis) = base.apk.execution_path(abi) {
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
    let signer = apps.join(format!(".{package}.signer"));
    let fingerprints = base.signature.fingerprints();
    match std::fs::read_to_string(&signer) {
        Ok(installed) => {
            let installed: Vec<&str> = installed.lines().collect();
            if installed != fingerprints {
                return Err(format!(
                    "{} is installed signed by a different certificate, so this update is refused: installed {}, update {}",
                    package.escape_debug(),
                    installed.join(", ").escape_debug(),
                    fingerprints.join(", ")
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read {}: {error}", signer.display())),
    }
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
    let staged_signer = apps.join(format!(".{package}.new-signer-{pid}"));
    let pending = app_dir.join(format!(".abi-{pid}"));
    let old_libraries = app_dir.join(format!(".old-lib-{pid}"));
    let old_code = apps.join(format!(".{package}.old-{pid}"));
    let old_signer = apps.join(format!(".{package}.old-signer-{pid}"));
    let record = app_dir.join("abi");
    let runtime = framework_runtime();
    let with_code: Vec<&str> = plan
        .chosen
        .iter()
        .filter(|(index, _)| parts[*index].apk.dex.files > 0)
        .map(|(index, _)| split_name(&parts[*index]))
        .collect();

    let result = (|| {
        std::fs::create_dir_all(&staging).map_err(|error| cannot(&staging, error))?;
        let mut count =
            extract_native_libs(&base.path, abi, &staging).map_err(|error| error.to_string())?;
        for (index, _) in &plan.chosen {
            count += extract_native_libs(&parts[*index].path, abi, &staging)
                .map_err(|error| error.to_string())?;
        }
        // Splits are part of the app even when nothing is prepared, so they get a code directory.
        let staged = matches!(runtime, Framework::Found(_)) || !plan.chosen.is_empty();
        if staged {
            stage_code(&staged_code, &parts, &plan)?;
        }
        let prepared = match &runtime {
            Framework::Found(runtime) => Some(
                prepare(runtime, &staged_code, &code, data, &with_code).map_err(|error| {
                    format!(
                        "cannot prepare {} for the framework: {error}",
                        package.escape_debug()
                    )
                })?,
            ),
            Framework::Missing(_) | Framework::Unknown(_) => None,
        };
        std::fs::write(&pending, format!("{abi}\n")).map_err(|error| cannot(&pending, error))?;
        let mut fingerprint_record = fingerprints.join("\n");
        fingerprint_record.push('\n');
        std::fs::write(&staged_signer, fingerprint_record)
            .map_err(|error| cannot(&staged_signer, error))?;

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
            if staged {
                renames.rename(&staged_code, &code)?;
            }
            if signer.exists() {
                renames.rename(&signer, &old_signer)?;
            }
            renames.rename(&staged_signer, &signer)?;
            renames.rename(&pending, &record)
        })();
        if let Err(error) = swap {
            renames.undo();
            return Err(error);
        }
        Ok((count, prepared))
    })();
    let _ = std::fs::remove_file(&pending);
    let _ = std::fs::remove_file(&staged_signer);
    let _ = std::fs::remove_file(&old_signer);
    for leftover in [&staging, &staged_code, &old_libraries, &old_code] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    drop(unpacked);
    let (count, prepared) = result?;

    let mut report = format!(
        "installed {}: {count} native libraries for {abi} in {}\n",
        package.escape_debug(),
        libraries.display()
    );
    if !plan.chosen.is_empty() || !plan.skipped.is_empty() {
        let chosen: Vec<(String, Kind)> = plan
            .chosen
            .iter()
            .map(|(index, kind)| (split_name(&parts[*index]).to_string(), kind.clone()))
            .collect();
        report.push_str(&format!(
            "installed splits of {}: {}",
            package.escape_debug(),
            if chosen.is_empty() {
                "none".to_string()
            } else {
                describe(&chosen)
            }
        ));
        if !plan.skipped.is_empty() {
            report.push_str(&format!("; skipped {}", describe(&plan.skipped)));
        }
        report.push('\n');
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
