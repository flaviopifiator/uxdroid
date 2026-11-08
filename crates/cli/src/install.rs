//! `uxdroid install`: the installer's part for an app the framework launcher starts. It verifies the
//! signature of every APK given, and that an update is signed as the installed app is. It puts the
//! APK's native libraries where `ApplicationInfo.nativeLibraryDir` points, `<data>/data/<package>/lib`,
//! and records the ABI it chose. An app delivered as a base APK plus splits, as files or as a bundle,
//! gets the splits that fit this machine, as `split_<name>.apk` beside `base.apk` in its code
//! directory, `<data>/app/<package>`. With the framework's runtime at hand it also prepares that
//! directory as Android's installer does: the DEX compiled beside the APKs, the framework's parse of
//! the manifest, and the library of default bodies. A start then only maps what is there.
//!
//! Every APK or bundle given is first copied into a directory of the install's own, and only that
//! copy is verified, read and installed, so the bytes verified are the bytes installed even when the
//! file given changes meanwhile. An app from a store source also has its copy checked against the
//! SHA-256, package, version and signers its source gave, and its source recorded.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use uxdroid_apk::{
    ExecutionPath, Host, INSTALLED_DATA, Kind, Part, Plan, Scheme, appearance, bundle_entries,
    extract_bundled, extract_native_libs, host_abi, inspect, plan_splits, verify_signature,
};

use sha2::{Digest, Sha256};
use uxdroid_store::Installed;

use crate::desktop::Menu;
use crate::runtime::{
    INSTRUCTION_SET, boot_class_path, default_runtime, failure, runtime_environment,
};
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
pub(crate) const HOST_DENSITY: u32 = 160;
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
    match install(&apks, data, &Origin::File) {
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
pub(crate) fn lock_package(data: &Path, package: &str) -> Result<std::fs::File, String> {
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

/// Moves this install's own copy `from` to `to`, or copies it where it cannot move.
fn move_apk(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).or_else(|_| copy_apk(from, to))
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
    move_apk(&parts[plan.base].path, &staged.join("base.apk"))?;
    let mut record = String::new();
    for (index, _) in &plan.chosen {
        let name = split_name(&parts[*index]);
        move_apk(
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
    std::fs::create_dir_all(staged.join("oat").join(INSTRUCTION_SET))
        .map_err(|error| cannot(staged, error))?;

    std::fs::create_dir_all(data.join("dalvik-cache").join(INSTRUCTION_SET))
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
            .arg(format!("--instruction-set={INSTRUCTION_SET}"))
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
                staged
                    .join("oat")
                    .join(INSTRUCTION_SET)
                    .join(format!("{file}.odex"))
                    .display()
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

/// A directory of `<data>/app` that holds this install's copies of the APKs and bundles given, and
/// the APKs unpacked from bundles, while it runs. The lock on its `lock` file says it is in use, so
/// an install can tell what a dead one left.
struct Unpacked {
    directory: PathBuf,
    /// `<data>` and `<data>/app` when this install made them: a refused install leaves no trace.
    created: Vec<PathBuf>,
    _lock: std::fs::File,
}

impl Unpacked {
    fn new(data: &Path) -> Result<Self, String> {
        let apps = data.join("app");
        let created: Vec<PathBuf> = [data.to_path_buf(), apps.clone()]
            .into_iter()
            .filter(|directory| !directory.exists())
            .collect();
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
        // Only this user may change the copies while they are verified and installed.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| cannot(&staging, error))?;
        let path = staging.join("lock");
        let lock = std::fs::File::create(&path).map_err(|error| cannot(&path, error))?;
        lock.lock()
            .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
        let directory = apps.join(format!(".{name}"));
        std::fs::rename(&staging, &directory).map_err(|error| cannot(&directory, error))?;
        Ok(Self {
            directory,
            created,
            _lock: lock,
        })
    }
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
        // Only when still empty, that is when the install was refused before it wrote anything.
        for directory in self.created.iter().rev() {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

/// Opens each APK given, so one that cannot be read is reported before anything is written.
/// The most this install copies of one file given: the size of the largest entry a bundle may
/// unpack, far beyond any real APK. Anything larger, such as `/dev/zero`, is refused.
const MAX_GIVEN_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn too_large(apk: &Path) -> String {
    format!(
        "{} is larger than the {MAX_GIVEN_BYTES} bytes an APK or bundle may have, so it is refused",
        apk.display()
    )
}

/// Opens each APK given, so one that cannot be read, or a file too large to be one, is reported
/// before anything is written.
fn open_all(apks: &[&Path]) -> Result<Vec<std::fs::File>, String> {
    apks.iter()
        .map(|apk| {
            let file = std::fs::File::open(apk)
                .map_err(|error| format!("cannot read APK {}: {error}", apk.display()))?;
            if file
                .metadata()
                .is_ok_and(|metadata| metadata.len() > MAX_GIVEN_BYTES)
            {
                return Err(too_large(apk));
            }
            Ok(file)
        })
        .collect()
}

/// Copies `source`, opened from `from`, to the new file `to` and returns the SHA-256 of what was
/// copied.
fn copy_hashing(mut source: std::fs::File, from: &Path, to: &Path) -> Result<String, String> {
    let mut copy = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|error| cannot(to, error))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    let mut total = 0u64;
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| format!("cannot read APK {}: {error}", from.display()))?;
        if read == 0 {
            break;
        }
        // A file that is not regular, such as a device or a pipe, has no size to check first.
        total += read as u64;
        if total > MAX_GIVEN_BYTES {
            return Err(too_large(from));
        }
        hash.update(&buffer[..read]);
        copy.write_all(&buffer[..read])
            .map_err(|error| cannot(to, error))?;
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Copies every APK given into `unpacked`, then reads and verifies the copies, unpacking bundles
/// there too. An app from a source must be the files downloaded, each with the SHA-256 it had then.
fn gather(
    apks: &[&Path],
    files: Vec<std::fs::File>,
    unpacked: &Unpacked,
    origin: &Origin,
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
    let directory = &unpacked.directory;
    for (number, (apk, file)) in apks.iter().zip(files).enumerate() {
        let copy = directory.join(format!("given-{number}"));
        let sha256 = copy_hashing(file, apk, &copy)?;
        if let Origin::Store(from) = origin {
            let expected = from.sha256s.get(number).map_or("none", String::as_str);
            if sha256 != expected {
                return Err(format!(
                    "the APK of {} from {} changed after it was downloaded (SHA-256 {sha256}, the index gives {expected}), so it is refused",
                    from.package, from.record.source
                ));
            }
        }
        let Some(entries) = bundle_entries(&copy).map_err(|error| match error {
            uxdroid_apk::Error::Splits(reason) => format!("{}: {reason}", apk.display()),
            uxdroid_apk::Error::Unreadable(_, source) => {
                format!("cannot read APK {}: {source}", apk.display())
            }
            other => other.to_string(),
        })?
        else {
            parts.push(read(apk.display().to_string(), copy)?);
            continue;
        };
        for entry in entries {
            let path = directory.join(format!("{}.apk", parts.len()));
            extract_bundled(&copy, &entry, &path).map_err(|error| error.to_string())?;
            parts.push(read(
                format!("{}!{}", apk.display(), entry.escape_debug()),
                path,
            )?);
        }
        let _ = std::fs::remove_file(&copy);
    }
    Ok(parts)
}

/// What an app from a store source must be, as its source signed it.
pub(crate) struct FromStore<'a> {
    pub package: &'a str,
    /// The SHA-256 each file had when it was downloaded, in the order given: a base, then splits.
    pub sha256s: &'a [String],
    /// The SHA-256 of each certificate the source says signs it; none when it does not say.
    pub signers: &'a [String],
    pub record: &'a Installed,
}

/// Where the APKs installed come from.
pub(crate) enum Origin<'a> {
    /// Files the user gave.
    File,
    /// A download from a store source, recorded with the install.
    Store(FromStore<'a>),
}

/// Checks the app the parts make is the one the source described.
fn check_origin(base: &Part, origin: &Origin) -> Result<(), String> {
    let Origin::Store(from) = origin else {
        return Ok(());
    };
    let manifest = &base.apk.manifest;
    let source = &from.record.source;
    if manifest.package != from.package {
        return Err(format!(
            "the APK of {} from {source} is the package {}, so it is refused",
            from.package,
            manifest.package.escape_debug()
        ));
    }
    let code = manifest
        .version_code
        .as_deref()
        .and_then(|code| code.parse::<u64>().ok());
    if code != Some(from.record.version_code) {
        return Err(format!(
            "the APK of {} from {source} has version code {}, and its index gives {}, so it is refused",
            from.package,
            manifest
                .version_code
                .as_deref()
                .unwrap_or("none")
                .escape_debug(),
            from.record.version_code
        ));
    }
    if !from.signers.is_empty() {
        let mut expected: Vec<String> = from.signers.to_vec();
        expected.sort();
        expected.dedup();
        let actual = base.signature.fingerprints();
        // A source may name the certificate a rotated key started from, as Google Play does, as
        // long as the proof of rotation has not revoked it.
        let rotated = matches!(&expected[..], [one] if base.signature.descends_from(one, 0));
        if actual != expected && !rotated {
            return Err(format!(
                "the APK of {} from {source} is signed by {}, and its index says {}, so it is refused",
                from.package,
                actual.join(", "),
                expected.join(", ")
            ));
        }
    }
    Ok(())
}

/// The languages of this user's locale, as `LANGUAGE` and the locale variables name them; English
/// when none is set.
pub(crate) fn host_languages() -> Vec<String> {
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

/// The ABI whose code this host runs through the translator, if it has one.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn guest_abi() -> Option<&'static str> {
    Some(crate::translation::GUEST_ABI)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn guest_abi() -> Option<&'static str> {
    None
}

/// The guest ABI an APK's code can run as on this host, through the translator.
fn translated_abi(abis: &[String]) -> Option<&'static str> {
    guest_abi().filter(|guest| abis.iter().any(|abi| abi == guest))
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
    Ok(summary.describe(&installed, package))
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn plan_translation(_staging: &Path, _package: &str) -> Result<String, String> {
    Err("translation is not built for this host".to_string())
}

pub(crate) fn install(apks: &[&Path], data: &Path, origin: &Origin) -> Result<Report, String> {
    if !data.is_absolute() {
        return Err(format!(
            "the data directory must be absolute, not {}",
            data.display()
        ));
    }
    let files = open_all(apks)?;
    let unpacked = Unpacked::new(data)?;
    let parts = gather(apks, files, &unpacked, origin)?;
    let native = host_abi();
    let host = Host {
        abi: native.to_string(),
        translated: guest_abi().map(str::to_string),
        density: HOST_DENSITY,
        languages: host_languages(),
    };
    let plan = plan_splits(&parts, &host).map_err(|error| error.to_string())?;
    let base = &parts[plan.base];
    let package = &base.apk.manifest.package;
    if !valid_package(package) {
        return Err(format!("bad package name {}", package.escape_debug()));
    }
    check_origin(base, origin)?;
    // An ABI split chosen decides the ABI, translated when it is not this host's; otherwise the
    // base APK's own libraries do.
    let split_abi = plan.chosen.iter().find_map(|(_, kind)| match kind {
        Kind::Abi(abi) => Some(abi.clone()),
        _ => None,
    });
    let (abi, translated) = match split_abi {
        Some(abi) => {
            let translated = abi != native;
            (abi, translated)
        }
        None => match base.apk.execution_path(native) {
            ExecutionPath::NeedsAbiBridge(abis) => match translated_abi(&abis) {
                Some(guest) => (guest.to_string(), true),
                None => {
                    return Err(format!(
                        "this APK has native code only for {}, and running it on {native} needs translation, which is not built yet",
                        abis.join(", ").escape_debug()
                    ));
                }
            },
            _ => (native.to_string(), false),
        },
    };

    // Read before the APKs move into place; only an app the menu will show needs it.
    let menu = Menu::of(data);
    let launcher = base.apk.manifest.launcher.clone();
    let look = match (&menu, &launcher) {
        (Some(_), Some(_)) => {
            let mut apks = vec![base.path.as_path()];
            apks.extend(
                plan.chosen
                    .iter()
                    .map(|(index, _)| parts[*index].path.as_path()),
            );
            appearance(&base.apk.manifest, &apks)
        }
        _ => Default::default(),
    };

    let _lock = lock_package(data, package)?;
    let pid = std::process::id();
    let app_dir = data.join("data").join(package);
    let libraries = app_dir.join("lib");
    let apps = data.join("app");
    let code = apps.join(package);
    let signer = apps.join(format!(".{package}.signer"));
    let source = Installed::path(data, package);
    let fingerprints = base.signature.fingerprints();
    match std::fs::read_to_string(&signer) {
        Ok(installed) => {
            let installed: Vec<&str> = installed.lines().collect();
            // As Android, an update by a rotated key is the same app when its proof of rotation
            // names the installed signer and lets the new key keep that app's data. Rotation is
            // for one signer only: signer sets otherwise compare exactly.
            let rotated = fingerprints.len() == 1
                && matches!(&installed[..], [one] if base.signature.descends_from(one, INSTALLED_DATA));
            if installed != fingerprints && !rotated {
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
    let staged_source = apps.join(format!(".{package}.new-source-{pid}"));
    let old_source = apps.join(format!(".{package}.old-source-{pid}"));
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
            extract_native_libs(&base.path, &abi, &staging).map_err(|error| error.to_string())?;
        for (index, _) in &plan.chosen {
            count += extract_native_libs(&parts[*index].path, &abi, &staging)
                .map_err(|error| error.to_string())?;
        }
        let translation = match translated {
            true => Some(plan_translation(&staging, package)?),
            false => None,
        };
        // Splits are part of the app even when nothing is prepared, so they get a code directory,
        // and so does a download from a store, which is deleted once installed: without it, the
        // app would have no APK to start from.
        let staged = matches!(runtime, Framework::Found(_))
            || !plan.chosen.is_empty()
            || matches!(origin, Origin::Store(_));
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
        if let Origin::Store(from) = origin {
            std::fs::write(&staged_source, from.record.to_bytes())
                .map_err(|error| cannot(&staged_source, error))?;
        }

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
            // An app installed from a file no longer comes from the source it came from.
            if source.exists() {
                renames.rename(&source, &old_source)?;
            }
            if let Origin::Store(_) = origin {
                renames.rename(&staged_source, &source)?;
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
    let _ = std::fs::remove_file(&staged_signer);
    let _ = std::fs::remove_file(&old_signer);
    let _ = std::fs::remove_file(&staged_source);
    let _ = std::fs::remove_file(&old_source);
    for leftover in [&staging, &staged_code, &old_libraries, &old_code] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    drop(unpacked);
    let (count, prepared, translation) = result?;

    let mut report = format!(
        "installed {}: {count} native libraries for {abi} in {}\n",
        package.escape_debug(),
        libraries.display()
    );
    if let Some(translation) = translation {
        report.push_str(&translation);
    }
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
    // The app is installed whatever happens to its menu entry, which only gets a note.
    let menu_note = menu.and_then(|menu| {
        menu.update(package, launcher.as_deref(), &look)
            .err()
            .map(|error| format!("{package} is not in the application menu: {error}"))
    });
    let note = match (note, menu_note) {
        (Some(note), Some(menu)) => Some(format!("{note}\nnote: {menu}")),
        (note, menu) => note.or(menu),
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
