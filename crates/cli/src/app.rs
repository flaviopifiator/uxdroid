//! `uxdroid app`: commands to an app started on the framework, through its control socket. An app
//! runs once per data directory; `start` hands its intent to the running process when there is one,
//! and starts the installed app in the background otherwise.

use std::ffi::OsString;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use uxdroid_system::control::client::{self, CallError};
use uxdroid_system::control::protocol::{Command as Request, Failure, Intent};
use uxdroid_system::control::{
    DEFAULT_PROFILE, PROFILE_VARIABLE, control_dir, instance_name, instance_socket, valid_package,
    valid_profile,
};

use crate::runtime::{art_flags, default_runtime, failure, home_dir, runtime_environment};
use crate::{EXIT_FAILURE, EXIT_USAGE, fail, print};

pub(crate) const APP_USAGE: &str = "usage: uxdroid app list\n       \
     uxdroid app <package> status [--profile <name>]\n       \
     uxdroid app <package> start [--activity <class>] [--action <action>] [--data <uri>] [--extra <key>=<value>]... [--flags <n>] [--profile <name>] [--data-dir <dir>] [--runtime <dir>]\n       \
     uxdroid app <package> intent [--activity <class>] [--action <action>] [--data <uri>] [--extra <key>=<value>]... [--flags <n>] [--profile <name>]\n       \
     uxdroid app <package> stop [--profile <name>]";
/// `status` and `intent` of an app that is not running.
const EXIT_NOT_RUNNING: u8 = 3;
/// Above the 10 s the app's main thread is given to answer.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
const LAUNCHER_CLASS: &str = "dev.uxdroid.system.Launcher";

/// One instance of an app: its package and the profile it runs as.
pub(crate) struct Instance {
    pub(crate) package: String,
    pub(crate) profile: String,
}

impl Instance {
    fn socket(&self, dir: &Path) -> Result<PathBuf, String> {
        instance_socket(dir, &self.package, &self.profile)
    }
}

enum Action {
    List,
    Status(Instance),
    Start(Instance, Intent, Option<PathBuf>, Option<PathBuf>),
    Intent(Instance, Intent),
    Stop(Instance),
}

/// Wrong usage, with what was wrong when there is more to say than the usage.
pub(crate) struct Usage(pub(crate) Option<String>);

/// A name that is wrong on its own is reported without the usage, which would not help.
pub(crate) fn names_a_bad_name(problem: &str) -> bool {
    problem.ends_with("is not a package name") || problem.ends_with("is not a profile name")
}

fn parse(args: &[OsString]) -> Result<Action, Usage> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Usage(None)))
        .collect::<Result<_, _>>()?;
    match args[..] {
        ["list"] => return Ok(Action::List),
        [package, ..] if !valid_package(package) && args.len() > 1 => {
            return Err(Usage(Some(format!("{package:?} is not a package name"))));
        }
        [package, command @ ("status" | "stop"), ref rest @ ..] => {
            let profile = match rest {
                [] => DEFAULT_PROFILE,
                ["--profile", profile] => checked_profile(profile)?,
                _ => return Err(Usage(None)),
            };
            let instance = Instance {
                package: package.into(),
                profile: profile.into(),
            };
            return Ok(if command == "status" {
                Action::Status(instance)
            } else {
                Action::Stop(instance)
            });
        }
        [package, command @ ("start" | "intent"), ..] => {
            let starting = command == "start";
            let (intent, profile, data, runtime) = options(&args[2..], starting)?;
            let instance = Instance {
                package: package.into(),
                profile,
            };
            return Ok(if starting {
                Action::Start(instance, intent, data, runtime)
            } else {
                Action::Intent(instance, intent)
            });
        }
        _ => {}
    }
    Err(Usage(None))
}

pub(crate) fn checked_profile(profile: &str) -> Result<&str, Usage> {
    if valid_profile(profile) {
        Ok(profile)
    } else {
        Err(Usage(Some(format!("{profile:?} is not a profile name"))))
    }
}

type Options = (Intent, String, Option<PathBuf>, Option<PathBuf>);

fn options(mut rest: &[&str], starting: bool) -> Result<Options, Usage> {
    let mut intent = Intent::default();
    let (mut data, mut runtime, mut flags, mut profile) = (None, None, None, None);
    while let [flag, value, remaining @ ..] = rest {
        let once = |slot: &mut Option<String>| {
            slot.is_none()
                .then(|| *slot = Some(value.to_string()))
                .ok_or(Usage(None))
        };
        match *flag {
            "--activity" => once(&mut intent.activity)?,
            "--action" => once(&mut intent.action)?,
            "--data" => once(&mut intent.data)?,
            "--extra" => {
                let (key, text) = value
                    .split_once('=')
                    .filter(|(key, _)| !key.is_empty())
                    .ok_or_else(|| {
                        Usage(Some(format!("--extra takes <key>=<value>, not {value:?}")))
                    })?;
                intent.extras.retain(|(existing, _)| existing != key);
                intent.extras.push((key.into(), text.into()));
            }
            "--flags" => once(&mut flags)?,
            "--profile" => {
                checked_profile(value)?;
                once(&mut profile)?;
            }
            "--data-dir" if starting => {
                let dir = PathBuf::from(value);
                if !dir.is_absolute() || data.is_some() {
                    return Err(Usage(Some(format!(
                        "--data-dir takes an absolute directory, not {value:?}"
                    ))));
                }
                data = Some(dir);
            }
            "--runtime" if starting && runtime.is_none() => runtime = Some(PathBuf::from(value)),
            _ => return Err(Usage(None)),
        }
        rest = remaining;
    }
    if !rest.is_empty() {
        return Err(Usage(None));
    }
    if let Some(flags) = flags {
        let parsed = match flags.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => flags.parse(),
        };
        intent.flags =
            parsed.map_err(|_| Usage(Some(format!("--flags takes a number, not {flags:?}"))))?;
    }
    // The checks the app applies, so a request it would refuse is wrong usage here.
    let checked =
        Intent::from_json(&intent.to_json()).map_err(|failure| Usage(Some(failure.message)))?;
    let profile = profile.unwrap_or_else(|| DEFAULT_PROFILE.into());
    Ok((checked, profile, data, runtime))
}

pub fn run_app(args: &[OsString]) -> ExitCode {
    let action = match parse(args) {
        Ok(action) => action,
        Err(Usage(Some(problem))) if names_a_bad_name(&problem) => {
            return fail(&format!("error: {problem}"), EXIT_USAGE);
        }
        Err(Usage(Some(problem))) => {
            return fail(&format!("error: {problem}\n{APP_USAGE}"), EXIT_USAGE);
        }
        Err(Usage(None)) => return fail(APP_USAGE, EXIT_USAGE),
    };
    let dir = match control_dir() {
        Ok(dir) => dir,
        Err(problem) => return fail(&format!("error: {problem}"), EXIT_FAILURE),
    };
    let result = match action {
        Action::List => list(&dir),
        Action::Status(app) => status(&dir, &app),
        Action::Start(app, intent, data, runtime) => start(&dir, &app, intent, data, runtime),
        Action::Intent(app, intent) => send_intent(&dir, &app, intent),
        Action::Stop(app) => stop(&dir, &app),
    };
    match result {
        Ok(Done::Printed(report)) => print(&report),
        Ok(Done::NotRunning(package, code)) => {
            let printed = print(&format!("{package} is not running\n"));
            if code == 0 {
                printed
            } else {
                ExitCode::from(code)
            }
        }
        Err(problem) => fail(&format!("error: {problem}"), EXIT_FAILURE),
    }
}

enum Done {
    Printed(String),
    NotRunning(String, u8),
}

/// App strings are escaped so they cannot forge lines of the report.
fn field<'a>(result: &'a Value, key: &str) -> std::borrow::Cow<'a, str> {
    match result.get(key) {
        Some(Value::String(text)) => text.escape_debug().to_string().into(),
        Some(Value::Number(number)) => number.to_string().into(),
        _ => "unknown".into(),
    }
}

fn call(
    dir: &Path,
    app: &Instance,
    request: &Request,
    timeout: Duration,
) -> Result<Option<Value>, String> {
    let package = &app.package;
    let socket = app.socket(dir)?;
    match client::call(&socket, request, timeout) {
        Ok(Ok(result)) => Ok(Some(result)),
        Ok(Err(Failure { code, message })) => Err(format!(
            "{package} refused: {} ({})",
            message.escape_debug(),
            code.escape_debug()
        )),
        Err(CallError::NotRunning) => Ok(None),
        Err(error) => Err(format!("{}: {error}", socket.display())),
    }
}

fn status(dir: &Path, app: &Instance) -> Result<Done, String> {
    let Some(result) = call(dir, app, &Request::Status, CALL_TIMEOUT)? else {
        return Ok(Done::NotRunning(app.package.clone(), EXIT_NOT_RUNNING));
    };
    let mut report = format!(
        "package: {}\npid: {}\ndata: {}\n",
        field(&result, "package"),
        field(&result, "pid"),
        field(&result, "data")
    );
    for activity in result
        .get("activities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        report.push_str(&format!(
            "activity: {} ({})\n",
            field(activity, "component"),
            field(activity, "state")
        ));
    }
    Ok(Done::Printed(report))
}

fn send_intent(dir: &Path, app: &Instance, intent: Intent) -> Result<Done, String> {
    let Some(result) = call(dir, app, &Request::Intent(intent), CALL_TIMEOUT)? else {
        return Ok(Done::NotRunning(app.package.clone(), EXIT_NOT_RUNNING));
    };
    Ok(Done::Printed(format!(
        "{} (process {}): {} {}\n",
        app.package,
        field(&result, "pid"),
        field(&result, "outcome"),
        field(&result, "activity")
    )))
}

/// Where a profile's apps are installed when no data directory is named: the default profile keeps
/// `$XDG_DATA_HOME/uxdroid/data`, any other is `$XDG_DATA_HOME/uxdroid/profiles/<profile>`.
pub(crate) fn profile_data(profile: &str) -> Result<PathBuf, String> {
    let base = home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid");
    Ok(if profile == DEFAULT_PROFILE {
        base.join("data")
    } else {
        base.join("profiles").join(profile)
    })
}

fn start(
    dir: &Path,
    app: &Instance,
    intent: Intent,
    data: Option<PathBuf>,
    runtime: Option<PathBuf>,
) -> Result<Done, String> {
    let package = app.package.as_str();
    if let Some(result) = call(dir, app, &Request::Start(intent.clone()), CALL_TIMEOUT)? {
        return Ok(Done::Printed(format!(
            "{package} is already running as process {}; the intent went to it: {} {}\n",
            field(&result, "pid"),
            field(&result, "outcome"),
            field(&result, "activity")
        )));
    }
    let data = match data {
        Some(data) => data,
        None => profile_data(&app.profile)?,
    };
    let apk = data.join("app").join(package).join("base.apk");
    if !apk.is_file() {
        return Err(format!(
            "{package} is not installed in {data}: run uxdroid install <apk> --data {data}",
            data = data.display()
        ));
    }
    let runtime = match runtime {
        Some(runtime) => runtime,
        None => default_runtime()?,
    };
    if !runtime.join("framework/framework.jar").is_file() {
        return Err(format!(
            "no framework in {}: build it, or name another with --runtime",
            runtime.display()
        ));
    }
    let log = dir.join(format!("{}.log", instance_name(package, &app.profile)));
    let mut child = background(
        launcher(&runtime, &data, &apk, &intent, &app.profile)?,
        &log,
    )?
    .spawn()
    .map_err(|error| format!("cannot start the runtime: {error}"))?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(exit) = child.try_wait().map_err(|error| error.to_string())? {
            if exit.success() {
                // Another start won the lock, and this one handed its intent to it.
                if let Some(result) = call(dir, app, &Request::Status, CALL_TIMEOUT)? {
                    return Ok(Done::Printed(format!(
                        "{package} is already running as process {}; the launch went to it (log in {})\n",
                        field(&result, "pid"),
                        log.display()
                    )));
                }
            }
            let written = std::fs::read(&log).unwrap_or_default();
            return Err(format!(
                "{package} exited during start with {exit}: {} (log in {})",
                failure(&written, &["Exception", "uxdroid: "]),
                log.display()
            ));
        }
        if let Some(result) = call(dir, app, &Request::Status, LIST_TIMEOUT)? {
            let top = result
                .get("activities")
                .and_then(Value::as_array)
                .and_then(|activities| activities.last())
                .map_or("no activity".into(), |top| {
                    field(top, "component").into_owned()
                });
            return Ok(Done::Printed(format!(
                "started {package} as process {}: {top}, log in {}\n",
                field(&result, "pid"),
                log.display()
            )));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "{package} did not serve its control socket within {} s (log in {})",
                START_TIMEOUT.as_secs(),
                log.display()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// The framework launcher for the installed APK, run as `profile`.
pub(crate) fn launcher(
    runtime: &Path,
    data: &Path,
    apk: &Path,
    intent: &Intent,
    profile: &str,
) -> Result<Command, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let native_dir = exe.parent().ok_or("cannot locate this program")?;
    let mut command = Command::new(runtime.join("bin/dalvikvm64"));
    command.args(art_flags(runtime));
    if native_dir.join("libuxdroid_nativebridge.so").is_file() {
        // App libraries linked against bionic load through the bridge.
        command.args([
            "-XX:NativeBridge=libuxdroid_nativebridge.so",
            "-Xforce-nb-testing",
        ]);
    }
    let class_path = format!(
        "{}:{}",
        runtime.join("framework/framework.jar").display(),
        runtime.join("framework/uxdroid-system.jar").display()
    );
    command.args(["-cp", &class_path, LAUNCHER_CLASS]);
    if *intent != Intent::default() {
        command.arg("--intent").arg(intent.to_json().to_string());
    }
    command
        .arg(runtime)
        .arg(native_dir.join("libuxdroid_system.so"))
        .arg(apk);
    let library_path = format!(
        "{}:{}",
        runtime.join("lib64").display(),
        native_dir.display()
    );
    runtime_environment(&mut command, runtime, data, &library_path);
    command.env(PROFILE_VARIABLE, profile);
    Ok(command)
}

/// `command` detached from this terminal, its output in `log`.
fn background(mut command: Command, log: &Path) -> Result<Command, String> {
    let output = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(log)
        .map_err(|error| format!("cannot write {}: {error}", log.display()))?;
    let errors = output.try_clone().map_err(|error| error.to_string())?;
    command.stdin(Stdio::null()).stdout(output).stderr(errors);
    // SAFETY: setsid is async-signal-safe; the app gets its own session, so closing this terminal
    // does not end it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    Ok(command)
}

fn alive(pid: i32) -> bool {
    // A zombie has exited; only its parent's wait is missing.
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false);
    // SAFETY: signal 0 only checks that the process exists.
    !zombie && unsafe { libc::kill(pid, 0) } == 0
}

fn stop(dir: &Path, app: &Instance) -> Result<Done, String> {
    let package = &app.package;
    let Some(result) = call(dir, app, &Request::Stop, CALL_TIMEOUT)? else {
        return Ok(Done::NotRunning(package.clone(), 0));
    };
    let pid = result
        .get("pid")
        .and_then(Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|&pid| pid > 0);
    let pid = pid.ok_or_else(|| format!("{package} answered stop without its process"))?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while alive(pid) {
        if Instant::now() > deadline {
            return Err(format!(
                "{package} (process {pid}) did not exit within {} s",
                STOP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
    Ok(Done::Printed(format!(
        "stopped {package} (process {pid}, {} activities finished)\n",
        field(&result, "finished")
    )))
}

fn list(dir: &Path) -> Result<Done, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            Some(
                entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".sock")?
                    .to_string(),
            )
        })
        .collect();
    names.sort();
    let mut report = String::new();
    for name in names {
        let (package, profile) = name.split_once('@').unwrap_or((&name, DEFAULT_PROFILE));
        // A profile named "default" is spelled by the package alone, so `<package>@default` is no one's.
        if !valid_package(package)
            || !valid_profile(profile)
            || instance_name(package, profile) != name
        {
            continue;
        }
        let app = Instance {
            package: package.into(),
            profile: profile.into(),
        };
        // An app that does not answer in time is left out, as one that ended is.
        if let Ok(Some(result)) = call(dir, &app, &Request::Status, LIST_TIMEOUT) {
            report.push_str(&format!(
                "{name} {} {}\n",
                field(&result, "pid"),
                field(&result, "data")
            ));
        }
    }
    Ok(Done::Printed(report))
}
