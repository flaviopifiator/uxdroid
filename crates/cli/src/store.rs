//! `uxdroid search`, `install <package>`, `update`, `remove`, `list` and `sources`: the command line
//! over `uxdroid_store`, which finds and downloads apps, and `install`, which installs them.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uxdroid_store::config::{Config, DEFAULT_REFRESH_AFTER_HOURS, PLAY};
use uxdroid_store::play::{Mode, Profile, Settings};
use uxdroid_store::{App, Device, Installed, Refresh, Release, Store, Want, valid_package};

use crate::install::{FromStore, HOST_DENSITY, Origin, host_languages, install, lock_package};
use crate::runtime::home_dir;
use crate::{EXIT_FAILURE, EXIT_USAGE, INSTALL_USAGE, fail, print};

pub(crate) const SEARCH_USAGE: &str =
    "usage: uxdroid search <text>... [--refresh] [--limit <n>] [--data <dir>]";
pub(crate) const UPDATE_USAGE: &str =
    "usage: uxdroid update [<package>] [--refresh] [--data <dir>]";
pub(crate) const REMOVE_USAGE: &str = "usage: uxdroid remove <package> [--data <dir>]";
pub(crate) const LIST_USAGE: &str = "usage: uxdroid list [--data <dir>]";
pub(crate) const SOURCES_USAGE: &str = "usage: uxdroid sources [--data <dir>]\n       \
     uxdroid sources add <name> <url> --fingerprint <sha256> [--data <dir>]\n       \
     uxdroid sources enable|disable <name> [--data <dir>]\n       \
     uxdroid sources refresh-after <hours> [--data <dir>]\n       \
     uxdroid sources play [--anonymous | --account <email>] [--dispenser <url>] [--data <dir>]";
const DEFAULT_LIMIT: usize = 20;
const NAME_WIDTH: usize = 32;

/// Command arguments: words, and options from `flags` (`true` when they take a value), each once.
pub(crate) struct Args {
    pub(crate) words: Vec<String>,
    options: Vec<(&'static str, Option<OsString>)>,
}

impl Args {
    pub(crate) fn parse(args: &[OsString], flags: &[(&'static str, bool)]) -> Option<Self> {
        let mut parsed = Args {
            words: Vec::new(),
            options: Vec::new(),
        };
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg.as_encoded_bytes().starts_with(b"--") {
                let (flag, takes) = flags.iter().find(|(flag, _)| arg == *flag)?;
                if parsed.options.iter().any(|(seen, _)| seen == flag) {
                    return None;
                }
                let value = if *takes {
                    Some(rest.next()?.clone())
                } else {
                    None
                };
                parsed.options.push((flag, value));
            } else {
                parsed.words.push(arg.to_str()?.to_string());
            }
        }
        Some(parsed)
    }

    fn has(&self, flag: &str) -> bool {
        self.options.iter().any(|(seen, _)| *seen == flag)
    }

    fn value(&self, flag: &str) -> Option<&OsString> {
        self.options
            .iter()
            .find(|(seen, _)| *seen == flag)
            .and_then(|(_, value)| value.as_ref())
    }

    fn refresh(&self) -> Refresh {
        if self.has("--refresh") {
            Refresh::Always
        } else {
            Refresh::IfStale
        }
    }

    /// `--data`, else the directory `uxdroid app` starts the default profile's apps from.
    pub(crate) fn data(&self) -> Result<PathBuf, String> {
        let data = match self.value("--data") {
            Some(data) => PathBuf::from(data),
            None => home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid/data"),
        };
        if !data.is_absolute() {
            return Err(format!(
                "the data directory must be absolute, not {}",
                data.display()
            ));
        }
        Ok(data)
    }
}

const INSTALL_FLAGS: &[(&str, bool)] = &[("--from", true), ("--refresh", false), ("--data", true)];

fn file_like(word: &str) -> bool {
    [".apk", ".apks", ".xapk"]
        .iter()
        .any(|extension| word.ends_with(extension))
}

/// Whether `uxdroid install` takes a package only because no file has that name, which the user is
/// told, since a mistyped file name would otherwise look like a package nobody offers.
fn package_for_want_of_a_file(parsed: &Args) -> bool {
    !parsed.has("--from") && !parsed.has("--refresh")
}

/// Whether `uxdroid install` was given a package to find in the sources rather than files: one word
/// that is a package name and no existing file, or `--from`.
pub(crate) fn names_a_package(args: &[OsString]) -> bool {
    let Some(parsed) = Args::parse(args, INSTALL_FLAGS) else {
        return false;
    };
    let [word] = &parsed.words[..] else {
        return false;
    };
    parsed.has("--from")
        || parsed.has("--refresh")
        || (!file_like(word) && valid_package(word) && !Path::new(word).exists())
}

/// Strings from a source are untrusted: control characters are shown escaped, so they cannot forge
/// lines or move the cursor.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub(crate) fn shorten(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut short: String = text.chars().take(width - 1).collect();
        short.push('…');
        short
    }
}

/// `YYYY-MM-DD` in UTC for milliseconds since the epoch.
pub(crate) fn date(milliseconds: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let days = (milliseconds / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Rows in columns two spaces apart, the last one not padded.
fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            if column + 1 == columns {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&" ".repeat(widths[column] - cell.chars().count() + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The store of `data`, telling the user about long downloads.
fn open_store(data: &Path) -> Result<Store, String> {
    let mut store = Store::open(data).map_err(|error| error.0)?;
    store.on_progress(Box::new(note));
    store.set_profile(profile());
    Ok(store)
}

/// The device Google Play is told about: this machine, and the density and languages the installer
/// keeps splits for.
pub(crate) fn profile() -> Profile {
    let device = Device::host();
    Profile {
        abi: device.abi,
        sdk: device.sdk,
        density: HOST_DENSITY,
        languages: host_languages(),
    }
}

fn note(message: &str) {
    let _ = fail(&format!("note: {message}"), 0);
}

pub(crate) fn version(name: &str, code: u64) -> String {
    if name.is_empty() {
        format!("({code})")
    } else {
        format!("{} ({code})", clean(name))
    }
}

fn more_apps(count: usize) -> String {
    if count == 1 {
        "1 more app matches".to_string()
    } else {
        format!("{count} more apps match")
    }
}

fn device_text(device: &Device) -> String {
    format!("{} and SDK {}", device.abi, device.sdk)
}

pub(crate) fn run_search(args: &[OsString]) -> ExitCode {
    let Some(parsed) = Args::parse(
        args,
        &[("--refresh", false), ("--limit", true), ("--data", true)],
    )
    .filter(|parsed| !parsed.words.is_empty()) else {
        return fail(SEARCH_USAGE, EXIT_USAGE);
    };
    let limit = match parsed.value("--limit") {
        None => DEFAULT_LIMIT,
        Some(value) => match value.to_str().and_then(|value| value.parse::<usize>().ok()) {
            Some(limit) if limit > 0 => limit,
            _ => return fail(SEARCH_USAGE, EXIT_USAGE),
        },
    };
    let text = parsed.words.join(" ");
    let result = (|| {
        let store = open_store(&parsed.data()?)?;
        let (apps, notes) = store
            .apps_for(None, parsed.refresh(), Want::Matching(&text))
            .map_err(|error| error.0)?;
        for message in &notes {
            note(message);
        }
        let device = Device::host();
        let matches = uxdroid_store::search(&apps, &text, &device);
        let mut rows = vec![vec![
            "NAME".to_string(),
            "PACKAGE".to_string(),
            "SOURCE".to_string(),
            "VERSION".to_string(),
            "UPDATED".to_string(),
        ]];
        for hit in matches.hits.iter().take(limit) {
            rows.push(vec![
                shorten(&clean(&hit.app.name), NAME_WIDTH),
                hit.app.package.clone(),
                hit.app.source.clone(),
                clean(&hit.release.version_name),
                hit.app
                    .updated
                    .or(hit.release.added)
                    .map_or("unknown".to_string(), date),
            ]);
        }
        if matches.hits.len() > limit {
            note(&format!(
                "{}; --limit <n> shows more",
                more_apps(matches.hits.len() - limit)
            ));
        }
        if matches.unfit > 0 {
            note(&format!(
                "{} but {} no version for {}",
                more_apps(matches.unfit),
                if matches.unfit == 1 { "has" } else { "have" },
                device_text(&device)
            ));
        }
        if matches.hits.is_empty() {
            return Err(format!("no app matches {}", clean(&text)));
        }
        Ok(table(&rows))
    })();
    match result {
        Ok(report) => print(&report),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// The app `package` in `apps`: from `from` when it is named, else from the source it was installed
/// from when that source has it, else from the first source that has it.
fn choose<'a>(apps: &'a [App], package: &str, installed: Option<&Installed>) -> Option<&'a App> {
    let mut offered = apps.iter().filter(|app| app.package == package);
    let first = offered.clone().next()?;
    Some(
        installed
            .and_then(|installed| offered.find(|app| app.source == installed.source))
            .unwrap_or(first),
    )
}

/// Why `app` has no release for `device`, naming its newest one.
pub(crate) fn unfit(app: &App, device: &Device) -> String {
    let newest = &app.releases[0];
    let mut needs = Vec::new();
    if !newest.abis.is_empty() && !newest.abis.contains(&device.abi) {
        needs.push(format!(
            "native code for {}",
            clean(&newest.abis.join(", "))
        ));
    }
    if newest.min_sdk > device.sdk {
        needs.push(format!("SDK {} or later", newest.min_sdk));
    }
    if let Some(max) = newest.max_sdk.filter(|max| *max < device.sdk) {
        needs.push(format!("SDK {max} or earlier"));
    }
    if !newest.stable {
        needs.push("a release channel other than stable".to_string());
    }
    format!(
        "{} in {} has no version for {}: its newest, {}, needs {}",
        app.package,
        app.source,
        device_text(device),
        version(&newest.version_name, newest.version_code),
        needs.join(" and ")
    )
}

/// Downloads `release` of `app` and installs it with its source recorded.
fn install_release(
    store: &Store,
    data: &Path,
    app: &App,
    release: &Release,
) -> Result<(String, Option<String>), String> {
    let download = store.download(app, release).map_err(|error| error.0)?;
    let record = Installed {
        source: app.source.clone(),
        version_code: release.version_code,
        version_name: release.version_name.clone(),
    };
    let sha256s: Vec<String> = download
        .files
        .iter()
        .map(|file| file.sha256.clone())
        .collect();
    let origin = Origin::Store(FromStore {
        package: &app.package,
        sha256s: &sha256s,
        signers: &release.signers,
        record: &record,
    });
    let (report, note) = install(&download.paths(), data, &origin)?;
    let what = match &download.files[..] {
        [file] => format!("{} bytes, SHA-256 {}", file.size, file.sha256),
        files => format!(
            "{} APKs ({}), {} bytes",
            files.len(),
            files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            download.size()
        ),
    };
    Ok((
        format!(
            "downloaded {} {} from {}: {what}\n{report}",
            app.package,
            version(&release.version_name, release.version_code),
            app.source,
        ),
        note,
    ))
}

pub(crate) fn run_install_package(args: &[OsString]) -> ExitCode {
    let Some(parsed) = Args::parse(args, INSTALL_FLAGS) else {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    };
    let [package] = &parsed.words[..] else {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    };
    if file_like(package) {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    }
    if package_for_want_of_a_file(&parsed) {
        note(&format!(
            "{package} is not a file, so it is looked up as a package name in the enabled sources"
        ));
    }
    let from = match parsed.value("--from").map(|from| from.to_str()) {
        None => None,
        Some(Some(from)) => Some(from.to_string()),
        Some(None) => return fail(INSTALL_USAGE, EXIT_USAGE),
    };
    let result = (|| {
        if !valid_package(package) {
            return Err(format!("bad package name {}", clean(package)));
        }
        let data = parsed.data()?;
        let store = open_store(&data)?;
        install_package(
            &store,
            &data,
            package,
            from.as_deref(),
            parsed.refresh(),
            &note,
        )
    })();
    report(result)
}

/// Installs `package` from `from`, or from the source it was installed from, or from the first
/// enabled source that has it: its newest release for this machine, unless that one is installed.
/// Notes about the sources go to `note` as they come.
pub(crate) fn install_package(
    store: &Store,
    data: &Path,
    package: &str,
    from: Option<&str>,
    refresh: Refresh,
    note: &dyn Fn(&str),
) -> Result<(String, Option<String>), String> {
    let installed = Installed::read(data, package).map_err(|error| error.0)?;
    let apps = offers(store, package, from, refresh, installed.as_ref(), note)?;
    let app = choose(&apps, package, installed.as_ref()).ok_or_else(|| match from {
        Some(from) => format!("{package} is not in {from}"),
        None => format!("no enabled source has {package}: uxdroid search finds apps"),
    })?;
    let device = Device::host();
    let release = app.newest(&device).ok_or_else(|| unfit(app, &device))?;
    if let Some(installed) = installed
        .as_ref()
        .filter(|installed| installed.source == app.source)
    {
        if installed.version_code == release.version_code {
            return Ok((
                format!(
                    "{package} {} from {} is already installed\n",
                    version(&installed.version_name, installed.version_code),
                    app.source
                ),
                None,
            ));
        }
        if installed.version_code > release.version_code {
            return Err(format!(
                "{package} {} is installed, newer than {} from {}, and Android does not downgrade apps",
                version(&installed.version_name, installed.version_code),
                version(&release.version_name, release.version_code),
                app.source
            ));
        }
    }
    install_release(store, data, app, release)
}

/// The enabled Google Play source, when there is one.
fn play_source(store: &Store) -> Option<String> {
    store
        .config()
        .play()
        .filter(|source| source.enabled)
        .map(|source| source.name.clone())
}

/// The offers of `package` that `install` chooses from. Without `from`, sources with an index come
/// first: Google Play is asked only when none has the package, or when it was installed from Play,
/// and the user is told when Play was not asked.
fn offers(
    store: &Store,
    package: &str,
    from: Option<&str>,
    refresh: Refresh,
    installed: Option<&Installed>,
    note: &dyn Fn(&str),
) -> Result<Vec<App>, String> {
    let wanted = [package.to_string()];
    let lookup = |from: Option<&str>| -> Result<Vec<App>, String> {
        let (apps, notes) = store
            .apps_for(from, refresh, Want::Packages(&wanted))
            .map_err(|error| error.0)?;
        for message in &notes {
            note(message);
        }
        Ok(apps)
    };
    let Some(play) = play_source(store).filter(|_| from.is_none()) else {
        return lookup(from);
    };
    if installed.is_some_and(|installed| installed.source == play) {
        return lookup(Some(&play));
    }
    let apps = match store.apps_for(None, refresh, Want::Everything) {
        Ok((apps, notes)) => {
            for message in &notes {
                note(message);
            }
            apps
        }
        Err(error) => {
            note(&error.0);
            Vec::new()
        }
    };
    match choose(&apps, package, installed) {
        Some(app) => {
            note(&format!(
                "{package} is in {}, so it is installed from there and Google Play is not asked; uxdroid install {package} --from {play} installs it from Google Play",
                app.source
            ));
            Ok(apps)
        }
        None => lookup(Some(&play)),
    }
}

fn report(result: Result<(String, Option<String>), String>) -> ExitCode {
    match result {
        Ok((report, message)) => {
            if let Some(message) = message {
                note(&message);
            }
            print(&report)
        }
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// Packages installed in `data`: a code directory, a signer record or native libraries.
pub(crate) fn installed_packages(data: &Path) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    let names = |directory: PathBuf| {
        std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    for name in names(data.join("app")) {
        let package = match name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".signer"))
        {
            Some(package) => package.to_string(),
            None if data.join("app").join(&name).join("base.apk").is_file() => name,
            None => continue,
        };
        if valid_package(&package) {
            packages.insert(package);
        }
    }
    for name in names(data.join("data")) {
        if valid_package(&name) && data.join("data").join(&name).join("abi").is_file() {
            packages.insert(name);
        }
    }
    packages
}

pub(crate) fn run_list(args: &[OsString]) -> ExitCode {
    let Some(parsed) =
        Args::parse(args, &[("--data", true)]).filter(|parsed| parsed.words.is_empty())
    else {
        return fail(LIST_USAGE, EXIT_USAGE);
    };
    let result = (|| -> Result<String, String> {
        let data = parsed.data()?;
        let listed = list(&data);
        if listed.is_empty() {
            return Ok(format!("no app is installed in {}\n", data.display()));
        }
        let mut rows = vec![vec![
            "PACKAGE".to_string(),
            "SOURCE".to_string(),
            "VERSION".to_string(),
        ]];
        for app in listed {
            rows.push(vec![app.package, app.source, app.version]);
        }
        Ok(table(&rows))
    })();
    match result {
        Ok(report) => print(&report),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// An installed app as `uxdroid list` shows it.
pub(crate) struct Listed {
    pub package: String,
    /// The source it came from, `file`, or `unknown` when its record cannot be read.
    pub source: String,
    pub version: String,
    /// Its record when it came from a source.
    pub record: Option<Installed>,
}

/// The apps installed in `data`, by package name.
pub(crate) fn list(data: &Path) -> Vec<Listed> {
    installed_packages(data)
        .into_iter()
        .map(|package| {
            let (source, installed_version, record) = match Installed::read(data, &package) {
                Ok(Some(record)) => (
                    record.source.clone(),
                    version(&record.version_name, record.version_code),
                    Some(record),
                ),
                Ok(None) => ("file".to_string(), apk_version(data, &package), None),
                Err(error) => ("unknown".to_string(), clean(&error.0), None),
            };
            Listed {
                package,
                source,
                version: installed_version,
                record,
            }
        })
        .collect()
}

/// The version in the installed `base.apk`'s manifest, when there is one.
fn apk_version(data: &Path, package: &str) -> String {
    let apk = data.join("app").join(package).join("base.apk");
    match uxdroid_apk::inspect(&apk) {
        Ok(apk) => {
            let code = apk.manifest.version_code.as_deref().unwrap_or("?");
            match apk.manifest.version_name.as_deref() {
                Some(name) => format!("{} ({})", clean(name), clean(code)),
                None => format!("({})", clean(code)),
            }
        }
        Err(_) => "unknown".to_string(),
    }
}

pub(crate) fn run_update(args: &[OsString]) -> ExitCode {
    let Some(parsed) = Args::parse(args, &[("--refresh", false), ("--data", true)])
        .filter(|parsed| parsed.words.len() <= 1)
    else {
        return fail(UPDATE_USAGE, EXIT_USAGE);
    };
    let data = match parsed.data() {
        Ok(data) => data,
        Err(error) => return fail(&format!("error: {error}"), EXIT_FAILURE),
    };
    let failed = std::cell::Cell::new(false);
    let fault = |message: String| {
        let _ = fail(&format!("error: {message}"), 0);
        failed.set(true);
    };
    let out = match update(
        &data,
        parsed.words.first().map(String::as_str),
        parsed.refresh(),
        &|| open_store(&data),
        &note,
        &fault,
    ) {
        Ok(out) => out,
        Err(error) => return fail(&format!("error: {error}"), EXIT_FAILURE),
    };
    let code = print(&out);
    if failed.get() {
        ExitCode::from(EXIT_FAILURE)
    } else {
        code
    }
}

/// Updates `named`, or every app installed from a source, to the newest release of its source for
/// this machine, and returns what was done. A failure that stops everything is an error; one that
/// stops a single app goes to `fault`, and notes about the sources to `note`. The store is opened
/// with `open` only when there is an app to update.
pub(crate) fn update(
    data: &Path,
    named: Option<&str>,
    refresh: Refresh,
    open: &dyn Fn() -> Result<Store, String>,
    note: &dyn Fn(&str),
    fault: &dyn Fn(String),
) -> Result<String, String> {
    let failed = std::cell::Cell::new(false);
    let fault = |message: String| {
        failed.set(true);
        fault(message);
    };
    let mut out = String::new();
    let packages: Vec<(String, Installed)> = match named {
        Some(package) => {
            if !valid_package(package) {
                return Err(format!("bad package name {}", clean(package)));
            }
            match Installed::read(data, package) {
                Ok(Some(record)) => vec![(package.to_string(), record)],
                Ok(None) if installed_packages(data).contains(package) => {
                    return Err(format!(
                        "{package} was installed from a file, so it has no source to update from: uxdroid install {package} --from <source> installs it from one"
                    ));
                }
                Ok(None) => return Err(format!("{package} is not installed")),
                Err(error) => return Err(error.0),
            }
        }
        None => installed_packages(data)
            .into_iter()
            .filter_map(|package| match Installed::read(data, &package) {
                Ok(Some(record)) => Some((package, record)),
                Ok(None) => None,
                Err(error) => {
                    fault(error.0);
                    None
                }
            })
            .collect(),
    };
    if packages.is_empty() {
        return Ok(if failed.get() {
            String::new()
        } else {
            "no app installed from a source\n".to_string()
        });
    }
    let store = open()?;
    let device = Device::host();
    // Each source is read once, for all its apps.
    let sources: BTreeSet<&str> = packages
        .iter()
        .map(|(_, record)| record.source.as_str())
        .collect();
    let mut offered: Vec<App> = Vec::new();
    for source in sources {
        let wanted: Vec<String> = packages
            .iter()
            .filter(|(_, record)| record.source == source)
            .map(|(package, _)| package.clone())
            .collect();
        match store.apps_for(Some(source), refresh, Want::Packages(&wanted)) {
            Ok((apps, notes)) => {
                notes.iter().for_each(|message| note(message));
                offered.extend(apps);
            }
            Err(error) => {
                for (package, record) in &packages {
                    if record.source == source {
                        fault(format!("cannot update {package}: {error}"));
                    }
                }
            }
        }
    }
    for (package, record) in &packages {
        let Some(app) = offered
            .iter()
            .find(|app| app.package == *package && app.source == record.source)
        else {
            if store
                .config()
                .source(&record.source)
                .is_some_and(|source| source.enabled)
            {
                fault(format!("{package} is no longer in {}", record.source));
            }
            continue;
        };
        let installed = version(&record.version_name, record.version_code);
        match app.newest(&device) {
            Some(release) if release.version_code > record.version_code => {
                match install_release(&store, data, app, release) {
                    Ok((report, message)) => {
                        if let Some(message) = message {
                            note(&message);
                        }
                        out.push_str(&report);
                        out.push_str(&format!(
                            "updated {package} from {installed} to {} from {}\n",
                            version(&release.version_name, release.version_code),
                            app.source
                        ));
                    }
                    Err(error) => fault(format!("cannot update {package}: {error}")),
                }
            }
            _ => out.push_str(&format!(
                "{package} {installed} from {} is up to date\n",
                record.source
            )),
        }
    }
    Ok(out)
}

pub(crate) fn run_remove(args: &[OsString]) -> ExitCode {
    let Some(parsed) = Args::parse(args, &[("--data", true)]) else {
        return fail(REMOVE_USAGE, EXIT_USAGE);
    };
    let [package] = &parsed.words[..] else {
        return fail(REMOVE_USAGE, EXIT_USAGE);
    };
    let result = (|| {
        if !valid_package(package) {
            return Err(format!("bad package name {}", clean(package)));
        }
        remove_package(&parsed.data()?, package)
    })();
    match result {
        Ok(report) => print(&report),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// Removes the installed app `package` and says what went.
pub(crate) fn remove_package(data: &Path, package: &str) -> Result<String, String> {
    if !installed_packages(data).contains(package) {
        return Err(format!("{package} is not installed in {}", data.display()));
    }
    remove(data, package)?;
    Ok(format!(
        "removed {package}: its code, data, signer record and component settings\n"
    ))
}

/// Removes what installs and starts of `package` wrote, under the package's install lock and its
/// instance lock, so neither an install nor a start runs meanwhile.
fn remove(data: &Path, package: &str) -> Result<(), String> {
    let _install = lock_package(data, package)?;
    let apps = data.join("app");
    let running = apps.join(format!(".{package}.running"));
    let _instance = match std::fs::File::open(&running) {
        Ok(file) => match file.try_lock() {
            Ok(()) => Some(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "{package} is running, so it is not removed: uxdroid app {package} stop stops it"
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("cannot lock {}: {error}", running.display()));
            }
        },
        Err(_) => None,
    };
    let cannot =
        |path: &Path, error: std::io::Error| format!("cannot remove {}: {error}", path.display());
    // The code goes first and at once, by a rename, so a start never finds half an app.
    let code = apps.join(package);
    if code.exists() {
        let old = apps.join(format!(".{package}.old-{}", std::process::id()));
        std::fs::rename(&code, &old).map_err(|error| cannot(&code, error))?;
        std::fs::remove_dir_all(&old).map_err(|error| cannot(&old, error))?;
    }
    for file in [
        apps.join(format!(".{package}.signer")),
        Installed::path(data, package),
        data.join("system/components").join(package),
    ] {
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(cannot(&file, error));
            }
            _ => {}
        }
    }
    let app_data = data.join("data").join(package);
    match std::fs::remove_dir_all(&app_data) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(cannot(&app_data, error)),
        _ => Ok(()),
    }
}

/// What `uxdroid sources enable play` says the first time.
pub(crate) const PLAY_WARNING: &str = "Google Play's terms allow only Google's own store app, so Google may limit or block the accounts that sign in with another client such as uxdroid. Anonymous mode signs in to shared accounts handed out by a third-party service, Aurora OSS's token dispenser (https://auroraoss.com), which uxdroid does not control and which may stop working at any time. uxdroid sources play --account <email> signs in with your own account instead, at your own risk.";

pub(crate) fn run_sources(args: &[OsString]) -> ExitCode {
    let Some(parsed) = Args::parse(
        args,
        &[
            ("--fingerprint", true),
            ("--anonymous", false),
            ("--account", true),
            ("--dispenser", true),
            ("--data", true),
        ],
    ) else {
        return fail(SOURCES_USAGE, EXIT_USAGE);
    };
    let words: Vec<&str> = parsed.words.iter().map(String::as_str).collect();
    let fingerprint = parsed
        .value("--fingerprint")
        .and_then(|value| value.to_str());
    let account = parsed.value("--account").map(|value| value.to_str());
    let dispenser = parsed.value("--dispenser").map(|value| value.to_str());
    let play_flags = parsed.has("--anonymous") || account.is_some() || dispenser.is_some();
    let valid = match words[..] {
        [] | ["enable", _] | ["disable", _] | ["refresh-after", _] => {
            !parsed.has("--fingerprint") && !play_flags
        }
        ["add", _, _] => fingerprint.is_some() && !play_flags,
        ["play"] => {
            !parsed.has("--fingerprint")
                && !(parsed.has("--anonymous") && account.is_some())
                && account.is_none_or(|account| account.is_some())
                && dispenser.is_none_or(|dispenser| dispenser.is_some())
        }
        _ => false,
    };
    if !valid {
        return fail(SOURCES_USAGE, EXIT_USAGE);
    }
    let hours = match words[..] {
        ["refresh-after", hours] => match refresh_hours(hours) {
            Some(hours) => Some(hours),
            None => return fail(SOURCES_USAGE, EXIT_USAGE),
        },
        _ => None,
    };
    let result = (|| -> Result<String, String> {
        let data = parsed.data()?;
        let mut store = open_store(&data)?;
        let message = match words[..] {
            ["add", name, url] => {
                let fingerprint = fingerprint.unwrap_or_default();
                store
                    .change(|config| config.add(name, url, fingerprint).map(|_| ()))
                    .map_err(|error| error.0)?;
                Some(format!("added {name}"))
            }
            ["enable", name] => {
                let mut added = false;
                store
                    .change(|config| {
                        if name == PLAY && config.source(PLAY).is_none() {
                            added = config.enable_play();
                            Ok(())
                        } else {
                            config.set_enabled(name, true)
                        }
                    })
                    .map_err(|error| error.0)?;
                if added {
                    let _ = fail(&format!("warning: {PLAY_WARNING}"), 0);
                }
                Some(format!("enabled {name}"))
            }
            ["disable", name] => {
                store
                    .change(|config| config.set_enabled(name, false))
                    .map_err(|error| error.0)?;
                Some(format!("disabled {name}"))
            }
            ["play"] => {
                return play_settings(
                    &store,
                    parsed.has("--anonymous"),
                    account.flatten(),
                    dispenser.flatten(),
                );
            }
            _ => None,
        };
        if let Some(hours) = hours {
            store
                .change(|config| {
                    config.refresh_after_hours = hours;
                    Ok(())
                })
                .map_err(|error| error.0)?;
            return Ok(format!(
                "indexes are refreshed when older than {hours} hours\n"
            ));
        }
        let mut out = message
            .map(|message| format!("{message}\n"))
            .unwrap_or_default();
        out.push_str(&sources_table(store.config()));
        if store.config().play().is_some() {
            out.push_str(&play_line(&store.play_settings().map_err(|error| error.0)?));
        }
        Ok(out)
    })();
    match result {
        Ok(report) => print(&report),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// How Google Play is signed in to, in one line.
fn play_line(settings: &Settings) -> String {
    match (settings.mode, &settings.email) {
        (Mode::Account, Some(email)) => format!("{PLAY} signs in as {email}\n"),
        _ => format!(
            "{PLAY} signs in anonymously through the token dispenser {}\n",
            settings.dispenser
        ),
    }
}

/// `uxdroid sources play`: shows or changes how Google Play is signed in to. An account's token is
/// read from standard input, never from the command line, which other users can see.
fn play_settings(
    store: &Store,
    anonymous: bool,
    account: Option<&str>,
    dispenser: Option<&str>,
) -> Result<String, String> {
    let settings = if let Some(email) = account {
        let token = read_token(email)?;
        let settings = store.play_account(email, &token).map_err(|error| error.0)?;
        if let Some(dispenser) = dispenser {
            store.play_dispenser(dispenser).map_err(|error| error.0)?;
        }
        settings
    } else if anonymous {
        store.play_anonymous(dispenser).map_err(|error| error.0)?
    } else if let Some(dispenser) = dispenser {
        store.play_dispenser(dispenser).map_err(|error| error.0)?
    } else {
        store.play_settings().map_err(|error| error.0)?
    };
    let mut out = play_line(&settings);
    if settings.mode == Mode::Account && account.is_some() {
        out.push_str("its token is kept readable only by you, and is never printed\n");
    }
    if store.config().play().is_none_or(|source| !source.enabled) {
        out.push_str(&format!(
            "{PLAY} is not enabled: uxdroid sources enable {PLAY} enables it\n"
        ));
    }
    Ok(out)
}

/// Reads one line from standard input, without echoing it when that is a terminal.
fn read_token(email: &str) -> Result<String, String> {
    use std::io::{BufRead, Read};
    // SAFETY: isatty and the termios calls only read and set the attributes of descriptor 0.
    let terminal = unsafe { libc::isatty(0) } == 1;
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let hidden = terminal && unsafe { libc::tcgetattr(0, &mut saved) } == 0;
    if terminal {
        let _ = fail(
            &format!("paste the AAS or OAuth token of {email} and press Enter (it is not shown):"),
            0,
        );
    }
    if hidden {
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &quiet) };
    }
    let mut line = String::new();
    let read = std::io::stdin().lock().take(16 * 1024).read_line(&mut line);
    if hidden {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
    }
    read.map_err(|error| format!("cannot read the token: {error}"))?;
    let token = line.trim().to_string();
    if token.is_empty() {
        return Err(format!(
            "no token was given for {email}: give it on standard input, as README.md describes"
        ));
    }
    Ok(token)
}

/// An interval for `refresh-after`: whole hours, at most a year.
pub(crate) fn refresh_hours(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|hours| *hours <= 24 * 365)
}

fn sources_table(config: &Config) -> String {
    let mut rows = vec![vec![
        "NAME".to_string(),
        "KIND".to_string(),
        "ENABLED".to_string(),
        "URL".to_string(),
        "FINGERPRINT".to_string(),
    ]];
    for source in &config.sources {
        rows.push(vec![
            source.name.clone(),
            source.kind.to_string(),
            if source.enabled { "yes" } else { "no" }.to_string(),
            source.url.clone(),
            source.fingerprint.clone(),
        ]);
    }
    let mut out = table(&rows);
    if config.refresh_after_hours != DEFAULT_REFRESH_AFTER_HOURS {
        out.push_str(&format!(
            "indexes are refreshed when older than {} hours\n",
            config.refresh_after_hours
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400_000), "2000-02-29");
        assert_eq!(date(1_793_273_970_519), "2026-10-29");
    }

    #[test]
    fn control_characters_from_a_source_are_escaped() {
        assert_eq!(clean("a\nb\u{1b}[2J\u{202e}c"), "a\\nb\\u{1b}[2J\\u{202e}c");
        assert_eq!(clean("Tom's café"), "Tom's café");
    }
}
