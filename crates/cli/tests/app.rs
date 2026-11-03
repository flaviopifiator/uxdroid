//! `uxdroid app`: commands to a running app through its control socket. A running app is played here by
//! a control server of `uxdroid_system::control` with a scripted handler, or by a fake runtime whose
//! dalvikvm64 serves the socket; real apps are covered by runtime/framework/control_smoke.sh.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uxdroid_system::control::protocol::{Command as Request, Failure, Intent};
use uxdroid_system::control::server::{self, Bound, Handler, Info, Server};

const APP_USAGE: &str = "usage: uxdroid app list\n       \
     uxdroid app <package> status [--profile <name>]\n       \
     uxdroid app <package> start [--activity <class>] [--action <action>] [--data <uri>] [--extra <key>=<value>]... [--flags <n>] [--profile <name>] [--data-dir <dir>] [--runtime <dir>]\n       \
     uxdroid app <package> intent [--activity <class>] [--action <action>] [--data <uri>] [--extra <key>=<value>]... [--flags <n>] [--profile <name>]\n       \
     uxdroid app <package> stop [--profile <name>]\n";

/// A private runtime directory, as `$XDG_RUNTIME_DIR` is, and a home.
fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("app-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("run")).unwrap();
    std::fs::create_dir_all(path.join("home")).unwrap();
    std::fs::set_permissions(path.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn uxdroid(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(args)
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("HOME", dir.join("home"))
        .env_remove("XDG_DATA_HOME")
        .env_remove("UXDROID_RUNTIME")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A running app as its socket sees it: answers status, records what it is sent.
struct App {
    pid: u32,
    activities: Value,
    refuse: Option<Failure>,
    received: Mutex<Vec<Request>>,
    /// What stop ends, as the app's own process would end.
    process: Mutex<Option<std::process::Child>>,
}

impl Handler for App {
    fn handle(&self, command: Request) -> Result<Value, Failure> {
        self.received.lock().unwrap().push(command.clone());
        if let Some(failure) = &self.refuse {
            return Err(failure.clone());
        }
        match command {
            Request::Status => Ok(json!({
                "package": "com.example.app", "pid": self.pid, "data": "/srv/android data",
                "activities": self.activities,
            })),
            Request::Start(_) | Request::Intent(_) => Ok(json!({
                "outcome": "new_intent", "pid": self.pid, "activity": "com.example.app/.MainActivity",
            })),
            Request::Stop => {
                if let Some(mut child) = self.process.lock().unwrap().take() {
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        let _ = child.kill();
                        let _ = child.wait();
                    });
                }
                Ok(json!({"pid": self.pid, "finished": 2}))
            }
            Request::Hello => unreachable!("answered by the server"),
        }
    }
}

fn app(pid: u32) -> App {
    App {
        pid,
        activities: json!([
            {"component": "com.example.app/.MainActivity", "state": "paused"},
            {"component": "com.example.app/.DetailActivity", "state": "resumed"},
        ]),
        refuse: None,
        received: Mutex::new(Vec::new()),
        process: Mutex::new(None),
    }
}

fn serve(dir: &Path, package: &str, app: App) -> (Server, Arc<App>) {
    serve_profile(dir, package, "default", app)
}

fn serve_profile(dir: &Path, package: &str, profile: &str, app: App) -> (Server, Arc<App>) {
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let Bound::Listening(listener) = server::bind_instance(&control, package, profile).unwrap()
    else {
        panic!("the socket is taken");
    };
    let app = Arc::new(app);
    let info = Info {
        package: package.into(),
        pid: app.pid,
    };
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    (
        server::serve(listener, info, uid, app.clone()).unwrap(),
        app,
    )
}

#[test]
fn wrong_usage_exits_2() {
    let dir = scratch("usage");
    for args in [
        &["app"][..],
        &["app", "com.example.app"],
        &["app", "com.example.app", "restart"],
        &["app", "com.example.app", "status", "extra"],
        &["app", "com.example.app", "stop", "--activity", "x"],
        &["app", "com.example.app", "start", "--activity"],
        &["app", "com.example.app", "start", "--extra", "novalue"],
        &["app", "com.example.app", "start", "--extra", "=v"],
        &["app", "com.example.app", "start", "--flags", "lots"],
        &[
            "app",
            "com.example.app",
            "start",
            "--action",
            "a",
            "--action",
            "b",
        ],
        &["app", "com.example.app", "intent", "--data-dir", "/x"],
        &["app", "com.example.app", "start", "--activity", "com/evil"],
        &["app", "list", "extra"],
    ] {
        let output = uxdroid(&dir, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).ends_with(APP_USAGE),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let output = uxdroid(&dir, &["app", "../etc", "status"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).starts_with("error: \"../etc\" is not a package name\n"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_command_list_names_app() {
    let dir = scratch("usage-all");
    let output = uxdroid(&dir, &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output)
            .contains("\n       uxdroid app <package> status|start|intent|stop [<options>]\n")
    );
    assert!(stderr(&output).ends_with("\n       uxdroid app list\n"));
}

#[test]
fn status_of_a_running_app() {
    let dir = scratch("status");
    let (server, _) = serve(&dir, "com.example.app", app(4242));
    let output = uxdroid(&dir, &["app", "com.example.app", "status"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "package: com.example.app\n\
         pid: 4242\n\
         data: /srv/android data\n\
         activity: com.example.app/.MainActivity (paused)\n\
         activity: com.example.app/.DetailActivity (resumed)\n"
    );
    server.shutdown();
}

#[test]
fn an_app_that_is_not_running_exits_3() {
    let dir = scratch("not-running");
    for (command, code) in [("status", 3), ("intent", 3), ("stop", 0)] {
        let mut args = vec!["app", "com.example.app", command];
        if command == "intent" {
            args.extend(["--action", "a.b"]);
        }
        let output = uxdroid(&dir, &args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{command}: {}",
            stderr(&output)
        );
        assert_eq!(
            stdout(&output),
            "com.example.app is not running\n",
            "{command}"
        );
    }
    // A socket file left by an app that crashed is not a running app.
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    drop(std::os::unix::net::UnixListener::bind(control.join("com.example.app.sock")).unwrap());
    let output = uxdroid(&dir, &["app", "com.example.app", "status"]);
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn start_while_running_hands_the_intent_to_the_app() {
    let dir = scratch("start-running");
    let (server, app) = serve(&dir, "com.example.app", app(77));
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--activity",
            ".MainActivity",
            "--action",
            "com.example.PING",
            "--data",
            "example://x/1",
            "--extra",
            "n=1",
            "--extra",
            "text=a=b c",
            "--flags",
            "0x20000000",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app is already running as process 77; the intent went to it: new_intent com.example.app/.MainActivity\n"
    );
    let intent = Intent {
        activity: Some(".MainActivity".into()),
        action: Some("com.example.PING".into()),
        data: Some("example://x/1".into()),
        extras: vec![("n".into(), "1".into()), ("text".into(), "a=b c".into())],
        flags: 0x2000_0000,
    };
    assert_eq!(*app.received.lock().unwrap(), vec![Request::Start(intent)]);
    server.shutdown();
}

#[test]
fn intent_goes_to_the_running_app() {
    let dir = scratch("intent");
    let (server, app) = serve(&dir, "com.example.app", app(78));
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "intent",
            "--action",
            "android.intent.action.VIEW",
            "--data",
            "example://y",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app (process 78): new_intent com.example.app/.MainActivity\n"
    );
    let intent = Intent {
        action: Some("android.intent.action.VIEW".into()),
        data: Some("example://y".into()),
        ..Intent::default()
    };
    assert_eq!(*app.received.lock().unwrap(), vec![Request::Intent(intent)]);
    server.shutdown();
}

#[test]
fn a_refusal_of_the_app_exits_1() {
    let dir = scratch("refused");
    let mut refusing = app(79);
    refusing.refuse = Some(Failure::new(
        "no_such_activity",
        "com.example.app declares no activity com.example.app.Nope",
    ));
    let (server, _) = serve(&dir, "com.example.app", refusing);
    let output = uxdroid(
        &dir,
        &["app", "com.example.app", "intent", "--activity", ".Nope"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: com.example.app refused: com.example.app declares no activity com.example.app.Nope (no_such_activity)\n"
    );
    server.shutdown();
}

#[test]
fn stop_waits_until_the_process_is_gone() {
    let dir = scratch("stop");
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let mut stopping = app(child.id());
    stopping.process = Mutex::new(Some(child));
    let (server, app) = serve(&dir, "com.example.app", stopping);
    let output = uxdroid(&dir, &["app", "com.example.app", "stop"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "stopped com.example.app (process {}, 2 activities finished)\n",
            app.pid
        )
    );
    // SAFETY: kill with signal 0 only checks that the process exists.
    assert_ne!(
        unsafe { libc::kill(app.pid as i32, 0) },
        0,
        "the process is gone when stop returns"
    );
    server.shutdown();
}

#[test]
fn list_shows_each_running_app() {
    let dir = scratch("list");
    let output = uxdroid(&dir, &["app", "list"]);
    assert_eq!(
        (output.status.code(), stdout(&output)),
        (Some(0), String::new())
    );
    let (first, _) = serve(&dir, "org.second", app(2));
    let (second, _) = serve(&dir, "com.example.app", app(1));
    let control = dir.join("run/uxdroid");
    drop(std::os::unix::net::UnixListener::bind(control.join("com.crashed.sock")).unwrap());
    let output = uxdroid(&dir, &["app", "list"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app 1 /srv/android data\norg.second 2 /srv/android data\n"
    );
    first.shutdown();
    second.shutdown();
}

#[test]
fn two_profiles_of_one_package_run_at_once_each_through_its_own_socket() {
    let dir = scratch("profiles");
    let (default, first) = serve(&dir, "com.example.app", app(1));
    let (work, second) = serve_profile(&dir, "com.example.app", "work", app(2));
    let output = uxdroid(&dir, &["app", "com.example.app", "status"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("package: com.example.app\npid: 1\n"));
    let output = uxdroid(
        &dir,
        &["app", "com.example.app", "status", "--profile", "work"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("package: com.example.app\npid: 2\n"));
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "intent",
            "--profile",
            "work",
            "--action",
            "a.b",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app (process 2): new_intent com.example.app/.MainActivity\n"
    );
    let intent = Intent {
        action: Some("a.b".into()),
        ..Intent::default()
    };
    assert_eq!(
        second.received.lock().unwrap().last(),
        Some(&Request::Intent(intent))
    );
    assert_eq!(*first.received.lock().unwrap(), vec![Request::Status]);
    let output = uxdroid(&dir, &["app", "list"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app 1 /srv/android data\ncom.example.app@work 2 /srv/android data\n"
    );
    let output = uxdroid(
        &dir,
        &["app", "com.example.app", "status", "--profile", "home"],
    );
    assert_eq!(
        (output.status.code(), stdout(&output)),
        (Some(3), "com.example.app is not running\n".to_string())
    );
    default.shutdown();
    work.shutdown();
}

#[test]
fn bad_profiles_are_refused() {
    let dir = scratch("bad-profiles");
    for profile in ["../x", "a@b", ".hidden", ""] {
        let output = uxdroid(
            &dir,
            &["app", "com.example.app", "status", "--profile", profile],
        );
        assert_eq!(output.status.code(), Some(2), "{profile}");
        assert_eq!(
            stderr(&output),
            format!("error: {profile:?} is not a profile name\n")
        );
    }
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "stop",
            "--profile",
            "a",
            "--profile",
            "b",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).ends_with(APP_USAGE), "{}", stderr(&output));
}

#[test]
fn start_with_a_profile_uses_the_profile_data_and_socket() {
    let dir = scratch("start-profile");
    let runtime = fake_runtime(&dir);
    let data = dir.join("home/.local/share/uxdroid/profiles/work");
    std::fs::create_dir_all(data.join("app/com.example.app")).unwrap();
    std::fs::write(data.join("app/com.example.app/base.apk"), b"").unwrap();
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--profile",
            "work",
            "--runtime",
            runtime.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed = stdout(&output);
    let log = dir.join("run/uxdroid/com.example.app@work.log");
    assert!(
        printed.ends_with(&format!(
            ": com.example.app/.MainActivity, log in {}\n",
            log.display()
        )),
        "{printed}"
    );
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    assert!(
        calls.contains(&format!("ANDROID_DATA={}\n", data.display())),
        "{calls}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("profile.log")).unwrap(),
        "work\n"
    );
    assert!(dir.join("run/uxdroid/com.example.app@work.sock").exists());
    let pid: i32 = printed
        .strip_prefix("started com.example.app as process ")
        .and_then(|rest| rest.split(':').next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("{printed}"));
    // SAFETY: a plain signal to the fake app's process.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

/// A runtime whose dalvikvm64 logs how it was started, then serves the control socket as the
/// launcher does, answering every request with its status, until it is killed.
fn fake_runtime(dir: &Path) -> PathBuf {
    let runtime = dir.join("runtime");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    std::fs::create_dir_all(runtime.join("framework")).unwrap();
    std::fs::write(runtime.join("framework/framework.jar"), b"").unwrap();
    let script = runtime.join("bin/dalvikvm64");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {log}\necho \"ANDROID_DATA=$ANDROID_DATA\" >> {log}\n\
             echo \"$UXDROID_PROFILE\" > {profile}\n\
             case \"$UXDROID_PROFILE\" in default) name=com.example.app;; *) name=\"com.example.app@$UXDROID_PROFILE\";; esac\n\
             echo \"launcher says hello\"\n\
             exec python3 -I -c '{server}' \"$XDG_RUNTIME_DIR/uxdroid/$name.sock\"\n",
            log = dir.join("calls.log").display(),
            profile = dir.join("profile.log").display(),
            server = "import json, os, socket, sys\n\
                      s = socket.socket(socket.AF_UNIX)\n\
                      s.bind(sys.argv[1])\n\
                      s.listen()\n\
                      while True:\n\
                      \x20   c, _ = s.accept()\n\
                      \x20   for line in c.makefile():\n\
                      \x20       a = [{\"component\": \"com.example.app/.MainActivity\", \"state\": \"resumed\"}]\n\
                      \x20       r = {\"pid\": os.getpid(), \"package\": \"com.example.app\", \"data\": \"/d\", \"activities\": a}\n\
                      \x20       c.sendall((json.dumps({\"v\": 1, \"id\": 1, \"ok\": True, \"result\": r}) + chr(10)).encode())\n\
                      \x20   c.close()\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    runtime
}

fn installed(dir: &Path) -> PathBuf {
    let data = dir.join("data");
    std::fs::create_dir_all(data.join("app/com.example.app")).unwrap();
    std::fs::write(data.join("app/com.example.app/base.apk"), b"").unwrap();
    data
}

#[test]
fn start_when_not_running_launches_the_installed_app_in_the_background() {
    let dir = scratch("start");
    let runtime = fake_runtime(&dir);
    let data = installed(&dir);
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--extra",
            "k=v",
            "--data-dir",
            data.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed = stdout(&output);
    let pid: i32 = printed
        .strip_prefix("started com.example.app as process ")
        .and_then(|rest| rest.split(':').next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("{printed}"));
    let log = dir.join("run/uxdroid/com.example.app.log");
    assert_eq!(
        printed,
        format!(
            "started com.example.app as process {pid}: com.example.app/.MainActivity, log in {}\n",
            log.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "launcher says hello\n"
    );
    let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    let launcher = lines
        .iter()
        .position(|line| *line == "dev.uxdroid.system.Launcher")
        .expect(&calls);
    assert_eq!(
        &lines[launcher..],
        &[
            "dev.uxdroid.system.Launcher",
            "--intent",
            r#"{"extras":{"k":"v"}}"#,
            runtime.to_str().unwrap(),
            &format!(
                "{}/libuxdroid_system.so",
                Path::new(env!("CARGO_BIN_EXE_uxdroid"))
                    .parent()
                    .unwrap()
                    .display()
            ),
            &format!("{}/app/com.example.app/base.apk", data.display()),
            &format!("ANDROID_DATA={}", data.display()),
        ][..],
        "{calls}"
    );
    assert!(lines.contains(&"-Xnorelocate"), "{calls}");
    // SAFETY: plain signal calls on the fake app's process.
    unsafe {
        assert_eq!(libc::kill(pid, 0), 0, "the app outlives the command");
        libc::kill(pid, libc::SIGKILL);
    }
}

#[test]
fn start_names_what_is_missing() {
    let dir = scratch("start-missing");
    let runtime = fake_runtime(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--data-dir",
            data.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "error: com.example.app is not installed in {data}: run uxdroid install <apk> --data {data}\n",
            data = data.display()
        )
    );
    let data = installed(&dir);
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--data-dir",
            data.to_str().unwrap(),
            "--runtime",
            "/nonexistent",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: no framework in /nonexistent: build it, or name another with --runtime\n"
    );
    let output = uxdroid(
        &dir,
        &["app", "com.example.app", "start", "--data-dir", "relative"],
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn an_app_that_exits_during_start_is_reported() {
    let dir = scratch("start-dies");
    let runtime = fake_runtime(&dir);
    std::fs::write(
        runtime.join("bin/dalvikvm64"),
        "#!/bin/sh\necho 'Exception: cannot start' >&2\nexit 1\n",
    )
    .unwrap();
    let data = installed(&dir);
    let output = uxdroid(
        &dir,
        &[
            "app",
            "com.example.app",
            "start",
            "--data-dir",
            data.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "error: com.example.app exited during start with exit status: 1: Exception: cannot start (log in {})\n",
            dir.join("run/uxdroid/com.example.app.log").display()
        )
    );
}

#[test]
fn only_start_connects_to_a_socket_no_app_serves_yet() {
    use std::io::{BufRead, BufReader, Write};
    let dir = scratch("socket-unit");
    // A systemd socket unit's socket before the app's first start: a listener, and no instance
    // holding the ownership lock.
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let manager =
        std::os::unix::net::UnixListener::bind(control.join("com.example.app.sock")).unwrap();
    manager.set_nonblocking(true).unwrap();
    for (args, code) in [
        (&["app", "com.example.app", "status"][..], 3),
        (&["app", "com.example.app", "intent", "--action", "a.b"], 3),
        (&["app", "com.example.app", "stop"], 0),
    ] {
        let output = uxdroid(&dir, args);
        assert_eq!(
            (output.status.code(), stdout(&output)),
            (Some(code), "com.example.app is not running\n".to_string()),
            "{args:?}"
        );
        assert!(
            manager.accept().is_err(),
            "{args:?} connected, which would start the app"
        );
    }
    let output = uxdroid(&dir, &["app", "list"]);
    assert_eq!(
        (output.status.code(), stdout(&output)),
        (Some(0), String::new())
    );
    assert!(
        manager.accept().is_err(),
        "list connected, which would start the app"
    );
    // start connects: that is what starts the app on demand. The started app answers here.
    manager.set_nonblocking(false).unwrap();
    let app = std::thread::spawn(move || {
        let (stream, _) = manager.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream)
            .write_all(
                b"{\"v\":1,\"id\":1,\"ok\":true,\"result\":{\"outcome\":\"brought_forward\",\"pid\":9,\"activity\":\"com.example.app/.MainActivity\"}}\n",
            )
            .unwrap();
        line
    });
    let output = uxdroid(&dir, &["app", "com.example.app", "start"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "com.example.app is already running as process 9; the intent went to it: brought_forward com.example.app/.MainActivity\n"
    );
    assert!(app.join().unwrap().contains("\"cmd\":\"start\""));
}
