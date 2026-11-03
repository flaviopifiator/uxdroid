//! `uxdroid service`: an installed app as a systemd user service, started on demand through its
//! control socket. Each profile of a package gets a socket unit, which systemd listens on, and a
//! service unit, which runs the launcher in the foreground with that socket passed to it. The units
//! are written per package and profile, not from one template, because what differs between apps
//! (the data directory, the network the manifest asks for, memory limits, start at login) is most of
//! the service; their names still follow systemd's template syntax, `uxdroid-<package>@<profile>`.
//!
//! `systemctl` runs only with `--now`; otherwise the command to run is printed.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use uxdroid_system::control::protocol::{Command as Request, Intent};
use uxdroid_system::control::{DEFAULT_PROFILE, control_dir, instance_name, valid_package};

use crate::app::{
    Instance, Usage, call_running, checked_profile, installed, launcher, names_a_bad_name,
};
use crate::runtime::home_dir;
use crate::{EXIT_FAILURE, EXIT_USAGE, fail, print};

pub(crate) const SERVICE_USAGE: &str = "usage: uxdroid service enable <package> [--profile <name>] [--data-dir <dir>] [--runtime <dir>] [--memory-max <size>] [--memory-high <size>] [--cpu-weight <n>] [--on-login] [--now]\n       \
     uxdroid service disable <package> [--profile <name>] [--now]\n       \
     uxdroid service status <package> [--profile <name>]\n       \
     uxdroid service run <package> [--profile <name>] [--data-dir <dir>] [--runtime <dir>]";
/// `status` of a profile with no service.
const EXIT_NOT_ENABLED: u8 = 3;
/// Mirrors the launcher's exit status when it finds the app running outside its service.
const EXIT_RUNNING_OUTSIDE_SERVICE: u8 = 75;
/// The first line of every unit this command writes; a file without it is the user's own.
const MARKER: &str = "# Written by uxdroid service enable for ";
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
/// The longest unit name systemd accepts.
const MAX_UNIT_NAME: usize = 255;
/// The longest path a Unix socket address holds.
const MAX_SOCKET_PATH: usize = 107;
/// A manifest bigger than this is not read for its permissions.
const MAX_MANIFEST: u64 = 4 << 20;
const INTERNET: &str = "android.permission.INTERNET";

#[derive(Default)]
struct Options {
    profile: Option<String>,
    data: Option<PathBuf>,
    runtime: Option<PathBuf>,
    memory_max: Option<String>,
    memory_high: Option<String>,
    cpu_weight: Option<u32>,
    on_login: bool,
    now: bool,
}

enum Verb {
    Enable,
    Disable,
    Status,
    Run,
}

fn parse(args: &[OsString]) -> Result<(Verb, String, Options), Usage> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Usage(None)))
        .collect::<Result<_, _>>()?;
    let [verb, package, rest @ ..] = &args[..] else {
        return Err(Usage(None));
    };
    let verb = match *verb {
        "enable" => Verb::Enable,
        "disable" => Verb::Disable,
        "status" => Verb::Status,
        "run" => Verb::Run,
        _ => return Err(Usage(None)),
    };
    if !valid_package(package) {
        return Err(Usage(Some(format!("{package:?} is not a package name"))));
    }
    let allowed: &[&str] = match verb {
        Verb::Enable => &[
            "--profile",
            "--data-dir",
            "--runtime",
            "--memory-max",
            "--memory-high",
            "--cpu-weight",
            "--on-login",
            "--now",
        ],
        Verb::Disable => &["--profile", "--now"],
        Verb::Status => &["--profile"],
        Verb::Run => &["--profile", "--data-dir", "--runtime"],
    };
    let mut options = Options::default();
    let mut rest = rest;
    while let [flag, remaining @ ..] = rest {
        if !allowed.contains(flag) {
            return Err(Usage(None));
        }
        rest = remaining;
        match *flag {
            "--on-login" | "--now" => {
                let set = if *flag == "--now" {
                    &mut options.now
                } else {
                    &mut options.on_login
                };
                if *set {
                    return Err(Usage(None));
                }
                *set = true;
                continue;
            }
            _ => {}
        }
        let [value, remaining @ ..] = rest else {
            return Err(Usage(None));
        };
        rest = remaining;
        let fresh = |taken: bool| if taken { Err(Usage(None)) } else { Ok(()) };
        match *flag {
            "--profile" => {
                fresh(options.profile.is_some())?;
                options.profile = Some(checked_profile(value)?.into());
            }
            "--data-dir" => {
                fresh(options.data.is_some())?;
                let dir = PathBuf::from(value);
                if !dir.is_absolute() {
                    return Err(Usage(Some(format!(
                        "--data-dir takes an absolute directory, not {value:?}"
                    ))));
                }
                options.data = Some(dir);
            }
            "--runtime" => {
                fresh(options.runtime.is_some())?;
                options.runtime = Some(PathBuf::from(value));
            }
            "--memory-max" | "--memory-high" => {
                if !memory_size(value) {
                    return Err(Usage(Some(format!(
                        "{flag} takes bytes with an optional K, M, G or T, a percentage or infinity, not {value:?}"
                    ))));
                }
                let slot = if *flag == "--memory-max" {
                    &mut options.memory_max
                } else {
                    &mut options.memory_high
                };
                fresh(slot.is_some())?;
                *slot = Some(value.to_string());
            }
            "--cpu-weight" => {
                fresh(options.cpu_weight.is_some())?;
                let weight = value
                    .parse()
                    .ok()
                    .filter(|weight| (1..=10_000).contains(weight))
                    .ok_or_else(|| {
                        Usage(Some(format!(
                            "--cpu-weight takes a number from 1 to 10000, not {value:?}"
                        )))
                    })?;
                options.cpu_weight = Some(weight);
            }
            _ => return Err(Usage(None)),
        }
    }
    Ok((verb, package.to_string(), options))
}

/// A size `MemoryMax=` and `MemoryHigh=` take: bytes with an optional binary suffix, a percentage of
/// the machine's memory, or `infinity`.
fn memory_size(value: &str) -> bool {
    if value == "infinity" {
        return true;
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent.parse::<u8>().is_ok_and(|percent| percent <= 100)
            && percent.bytes().all(|byte| byte.is_ascii_digit());
    }
    let digits = value.strip_suffix(['K', 'M', 'G', 'T']).unwrap_or(value);
    !digits.is_empty() && digits.len() <= 19 && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn run_service(args: &[OsString]) -> ExitCode {
    let (verb, package, options) = match parse(args) {
        Ok(parsed) => parsed,
        Err(Usage(Some(problem))) if names_a_bad_name(&problem) => {
            return fail(&format!("error: {problem}"), EXIT_USAGE);
        }
        Err(Usage(Some(problem))) => {
            return fail(&format!("error: {problem}\n{SERVICE_USAGE}"), EXIT_USAGE);
        }
        Err(Usage(None)) => return fail(SERVICE_USAGE, EXIT_USAGE),
    };
    let app = Instance {
        package,
        profile: options
            .profile
            .clone()
            .unwrap_or_else(|| DEFAULT_PROFILE.into()),
    };
    let result = match verb {
        Verb::Enable => enable(&app, options),
        Verb::Disable => disable(&app, options.now),
        Verb::Status => status(&app),
        Verb::Run => return run(&app, options),
    };
    match result {
        Ok((report, code)) => {
            let printed = print(&report);
            if code == 0 {
                printed
            } else {
                ExitCode::from(code)
            }
        }
        Err(problem) => fail(&format!("error: {problem}"), EXIT_FAILURE),
    }
}

/// `uxdroid-<package>@<profile>`, the profile escaped as `systemd-escape` escapes an instance name.
fn unit_name(app: &Instance) -> String {
    format!("uxdroid-{}@{}", app.package, systemd_escape(&app.profile))
}

/// systemd's escaping of a string into a unit name: `/` becomes `-`, and every byte other than ASCII
/// letters, digits, `:`, `_` and a `.` that does not lead becomes `\xNN`. Valid profiles need none of
/// it; it keeps a name that reaches here from ever forming another unit's name.
fn systemd_escape(text: &str) -> String {
    let mut escaped = String::new();
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'/' => escaped.push('-'),
            b'.' if index > 0 => escaped.push('.'),
            byte if byte.is_ascii_alphanumeric() || byte == b':' || byte == b'_' => {
                escaped.push(byte as char)
            }
            byte => escaped.push_str(&format!("\\x{byte:02x}")),
        }
    }
    escaped
}

fn units_dir() -> Result<PathBuf, String> {
    let config = match std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        Some(config) => config,
        None => home_dir("XDG_CONFIG_HOME", ".config")?,
    };
    Ok(config.join("systemd/user"))
}

/// What `%t` is for the user manager: the runtime directory.
fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        // SAFETY: geteuid has no preconditions.
        _ => PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })),
    }
}

/// `path` as one word of a unit file, its `%` specifiers escaped; also `$` in a command line, where
/// systemd expands variables. Characters a word cannot hold without quoting are refused.
fn unit_word(path: &Path, command: bool) -> Result<String, String> {
    let text = path
        .to_str()
        .filter(|text| {
            !text
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\'))
        })
        .ok_or_else(|| {
            format!(
                "{} holds a space, a quote, a backslash or a control character, which a unit file cannot hold here",
                path.display()
            )
        })?;
    let text = text.replace('%', "%%");
    Ok(if command {
        text.replace('$', "$$")
    } else {
        text
    })
}

/// Undoes `unit_word` for a word read back from a unit this command wrote.
fn unescape(word: &str) -> String {
    word.replace("%%", "%")
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| format!("cannot find {}: {error}", path.display()))
}

/// Whether the manifest asks for `INTERNET`. The manifest's string pool is searched for the name as
/// a whole pool string, in either of binary XML's encodings: a permission the app requests is always
/// there, so an app that asks for it is never cut off, and an app whose manifest merely holds the
/// string elsewhere gets the network too.
fn asks_for_internet(apk: &Path) -> Result<bool, String> {
    let cannot = |error: &dyn std::fmt::Display| format!("cannot read {}: {error}", apk.display());
    let file = std::fs::File::open(apk).map_err(|error| cannot(&error))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| cannot(&error))?;
    let entry = archive
        .by_name("AndroidManifest.xml")
        .map_err(|error| cannot(&error))?;
    if entry.size() > MAX_MANIFEST {
        return Err(cannot(&"its manifest is too large"));
    }
    let mut manifest = Vec::new();
    entry
        .take(MAX_MANIFEST)
        .read_to_end(&mut manifest)
        .map_err(|error| cannot(&error))?;
    let length = INTERNET.len() as u8;
    // UTF-8 pool strings: character count, byte count, bytes, a zero.
    let mut utf8 = vec![length, length];
    utf8.extend_from_slice(INTERNET.as_bytes());
    utf8.push(0);
    // UTF-16 pool strings: a 16-bit count, the units, a zero unit.
    let mut utf16 = vec![length, 0];
    for byte in INTERNET.bytes() {
        utf16.extend_from_slice(&[byte, 0]);
    }
    utf16.extend_from_slice(&[0, 0]);
    let holds = |needle: &[u8]| {
        manifest
            .windows(needle.len())
            .any(|window| window == needle)
    };
    Ok(holds(&utf8) || holds(&utf16))
}

struct Settings {
    exe: String,
    data: String,
    runtime: String,
    internet: bool,
    memory_max: Option<String>,
    memory_high: Option<String>,
    cpu_weight: u32,
    on_login: bool,
}

fn header(app: &Instance) -> String {
    format!(
        "{MARKER}{}, profile {}.\n# uxdroid service disable {} removes it.\n",
        app.package, app.profile, app.package
    )
}

fn socket_unit(app: &Instance) -> String {
    let name = unit_name(app);
    format!(
        "{header}[Unit]\n\
         Description=Control socket of uxdroid app {package}, profile {profile}\n\
         \n\
         [Socket]\n\
         ListenStream=%t/uxdroid/{instance}.sock\n\
         SocketMode=0600\n\
         DirectoryMode=0700\n\
         FileDescriptorName={fdname}\n\
         Service={name}.service\n\
         RemoveOnStop=yes\n\
         \n\
         [Install]\n\
         WantedBy=sockets.target\n",
        header = header(app),
        package = app.package,
        profile = app.profile,
        instance = instance_name(&app.package, &app.profile),
        fdname = uxdroid_system::control::server::LISTEN_NAME,
    )
}

fn service_unit(app: &Instance, settings: &Settings) -> String {
    let name = unit_name(app);
    let mut limits = String::new();
    if let Some(high) = &settings.memory_high {
        limits.push_str(&format!("MemoryHigh={high}\n"));
    }
    if let Some(max) = &settings.memory_max {
        limits.push_str(&format!("MemoryMax={max}\n"));
    }
    limits.push_str(&format!("CPUWeight={}\n", settings.cpu_weight));
    let families = if settings.internet {
        "AF_UNIX AF_NETLINK AF_INET AF_INET6"
    } else {
        "AF_UNIX AF_NETLINK"
    };
    let install = if settings.on_login {
        "\n[Install]\nWantedBy=default.target\n"
    } else {
        ""
    };
    let in_command = |word: &str| word.replace('$', "$$");
    format!(
        "{header}[Unit]\n\
         Description=uxdroid app {package}, profile {profile}\n\
         Requires={name}.socket\n\
         After={name}.socket graphical-session.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=exec\n\
         ExecStart={exe} service run {package} --profile {profile} --data-dir {data_command} --runtime {runtime_command}\n\
         Environment=UXDROID_PROFILE={profile}\n\
         Environment=UXDROID_RUNTIME={runtime}\n\
         Environment=ANDROID_DATA={data}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         RestartPreventExitStatus={EXIT_RUNNING_OUTSIDE_SERVICE}\n\
         TimeoutStopSec=15\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         SyslogIdentifier={name}\n\
         {limits}\
         UMask=0077\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ReadWritePaths={data} %t/uxdroid\n\
         PrivateTmp=yes\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictNamespaces=yes\n\
         RestrictRealtime=yes\n\
         RestrictSUIDSGID=yes\n\
         LockPersonality=yes\n\
         SystemCallArchitectures=native\n\
         RestrictAddressFamilies={families}\n\
         {install}",
        header = header(app),
        package = app.package,
        profile = app.profile,
        exe = in_command(&settings.exe),
        data = settings.data,
        data_command = in_command(&settings.data),
        runtime = settings.runtime,
        runtime_command = in_command(&settings.runtime),
    )
}

/// The unit files of an instance, if they exist, refusing any this command did not write.
fn own_units(dir: &Path, app: &Instance) -> Result<[(PathBuf, bool); 2], String> {
    let name = unit_name(app);
    let mut units = [
        (dir.join(format!("{name}.socket")), false),
        (dir.join(format!("{name}.service")), false),
    ];
    for (path, exists) in &mut units {
        match std::fs::read(&*path) {
            Ok(contents) if contents.starts_with(MARKER.as_bytes()) => *exists = true,
            Ok(_) => {
                return Err(format!(
                    "{} was not written by uxdroid service enable; it is left as it is",
                    path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        }
    }
    Ok(units)
}

/// Writes `contents` to `path` through a file beside it, so a unit is never seen half written.
fn write_unit(path: &Path, contents: &str) -> Result<(), String> {
    let cannot = |error: std::io::Error| format!("cannot write {}: {error}", path.display());
    let temporary = path.with_extension(format!("uxdroid-{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&temporary)?;
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
        // The mode asked for above passes through the umask; units are read by the user manager only,
        // but must never be writable by others.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(cannot)
}

/// Runs `systemctl --user` with `args`, returning the line that reports it.
fn systemctl(args: &[&str]) -> Result<String, String> {
    let line = format!("systemctl --user {}", args.join(" "));
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .map_err(|error| format!("cannot run systemctl: {error}"))?;
    if !status.success() {
        return Err(format!("{line} failed with {status}"));
    }
    Ok(format!("ran: {line}\n"))
}

fn enable(app: &Instance, options: Options) -> Result<(String, u8), String> {
    let (data, apk, runtime) = installed(app, options.data, options.runtime)?;
    let data = canonical(&data)?;
    let runtime = canonical(&runtime)?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("cannot locate this program: {error}"))?;
    let name = unit_name(app);
    if name.len() + ".service".len() > MAX_UNIT_NAME {
        return Err(format!("{name}.service is longer than a unit name can be"));
    }
    let socket = runtime_dir().join("uxdroid").join(format!(
        "{}.sock",
        instance_name(&app.package, &app.profile)
    ));
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(format!(
            "{} is longer than a socket address can be",
            socket.display()
        ));
    }
    let settings = Settings {
        exe: unit_word(&exe, false)?,
        data: unit_word(&data, false)?,
        runtime: unit_word(&runtime, false)?,
        internet: asks_for_internet(&apk)?,
        memory_max: options.memory_max,
        memory_high: options.memory_high,
        cpu_weight: options.cpu_weight.unwrap_or(100),
        on_login: options.on_login,
    };
    let dir = units_dir()?;
    own_units(&dir, app)?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot make {}: {error}", dir.display()))?;
    let socket_file = dir.join(format!("{name}.socket"));
    let service_file = dir.join(format!("{name}.service"));
    write_unit(&socket_file, &socket_unit(app))?;
    write_unit(&service_file, &service_unit(app, &settings))?;
    let mut report = format!(
        "wrote {}\nwrote {}\n",
        socket_file.display(),
        service_file.display()
    );
    let socket_unit = format!("{name}.socket");
    let service_unit = format!("{name}.service");
    let mut commands: Vec<Vec<&str>> =
        vec![vec!["daemon-reload"], vec!["enable", "--now", &socket_unit]];
    if settings.on_login {
        commands.push(vec!["enable", &service_unit]);
    }
    if options.now {
        for command in &commands {
            report.push_str(&systemctl(command)?);
        }
    } else {
        let lines: Vec<String> = commands
            .iter()
            .map(|command| format!("systemctl --user {}", command.join(" ")))
            .collect();
        report.push_str(&format!(
            "to start {} on demand, run: {}\n",
            app.package,
            lines.join(" && ")
        ));
    }
    Ok((report, 0))
}

fn no_service(app: &Instance) -> String {
    format!(
        "{} has no service for profile {}\n",
        app.package, app.profile
    )
}

fn disable(app: &Instance, now: bool) -> Result<(String, u8), String> {
    let dir = units_dir()?;
    let units = own_units(&dir, app)?;
    if units.iter().all(|(_, exists)| !exists) {
        return Ok((no_service(app), 0));
    }
    let name = unit_name(app);
    let socket_unit = format!("{name}.socket");
    let service_unit = format!("{name}.service");
    let mut report = String::new();
    if now {
        report.push_str(&systemctl(&[
            "disable",
            "--now",
            &socket_unit,
            &service_unit,
        ])?);
    }
    // What `systemctl --user enable` linked, which would otherwise point at nothing.
    for (wants, unit) in [
        ("sockets.target.wants", &socket_unit),
        ("default.target.wants", &service_unit),
    ] {
        let link = dir.join(wants).join(unit);
        if std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()) {
            std::fs::remove_file(&link)
                .map_err(|error| format!("cannot remove {}: {error}", link.display()))?;
        }
    }
    let mut removed = String::new();
    for (path, exists) in &units {
        if *exists {
            std::fs::remove_file(path)
                .map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
            removed.push_str(&format!("removed {}\n", path.display()));
        }
    }
    if now {
        report.push_str(&systemctl(&["daemon-reload"])?);
        Ok((removed + &report, 0))
    } else {
        Ok((
            format!(
                "{removed}to stop what still runs, run: systemctl --user stop {socket_unit} {service_unit} && systemctl --user daemon-reload\n"
            ),
            0,
        ))
    }
}

fn status(app: &Instance) -> Result<(String, u8), String> {
    let dir = units_dir()?;
    let [_, (service_file, exists)] = own_units(&dir, app)?;
    if !exists {
        return Ok((no_service(app), EXIT_NOT_ENABLED));
    }
    let service = std::fs::read_to_string(&service_file)
        .map_err(|error| format!("cannot read {}: {error}", service_file.display()))?;
    let data = service
        .lines()
        .find_map(|line| line.strip_prefix("Environment=ANDROID_DATA="))
        .map_or("unknown".into(), unescape);
    let on_login = service
        .lines()
        .any(|line| line == "WantedBy=default.target");
    let name = unit_name(app);
    let control = control_dir()?;
    let socket = runtime_dir().join("uxdroid").join(format!(
        "{}.sock",
        instance_name(&app.package, &app.profile)
    ));
    // Only an instance that already runs is asked: a connection to the socket would start the app.
    let running = match call_running(&control, app, &Request::Status, STATUS_TIMEOUT) {
        Ok(Some(result)) => match result.get("pid").and_then(serde_json::Value::as_u64) {
            Some(pid) => format!("process {pid}"),
            None => "yes".into(),
        },
        Ok(None) => "no".into(),
        Err(problem) => format!("unknown ({problem})"),
    };
    Ok((
        format!(
            "service: {name}.service\n\
             socket unit: {name}.socket\n\
             units in: {}\n\
             socket: {}\n\
             data: {}\n\
             on login: {}\n\
             running: {running}\n",
            dir.display(),
            socket.display(),
            data.escape_debug(),
            if on_login { "yes" } else { "no" },
        ),
        0,
    ))
}

/// Runs the installed app in the foreground, replacing this process, as the service unit's command:
/// the launcher keeps this process id, so the socket systemd passed, which names that id, reaches it.
fn run(app: &Instance, options: Options) -> ExitCode {
    let started = installed(app, options.data, options.runtime).and_then(|(data, apk, runtime)| {
        launcher(&runtime, &data, &apk, &Intent::default(), &app.profile)
    });
    match started {
        Ok(mut command) => {
            let error = command.exec();
            fail(
                &format!("error: cannot start the runtime: {error}"),
                EXIT_FAILURE,
            )
        }
        Err(problem) => fail(&format!("error: {problem}"), EXIT_FAILURE),
    }
}
