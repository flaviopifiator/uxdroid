//! `uxdroid service`: systemd user units for an installed app. The containers have no systemd, so the
//! units are checked as written, `systemctl` is a fake on `PATH` that records how it was called, and
//! socket activation itself is covered by runtime/framework/service_smoke.sh.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use uxdroid_testkit::{ATTR_NAME, Apk, Value, android, sample_manifest};

const SERVICE_USAGE: &str = "usage: uxdroid service enable <package> [--profile <name>] [--data-dir <dir>] [--runtime <dir>] [--memory-max <size>] [--memory-high <size>] [--cpu-weight <n>] [--on-login] [--now]\n       \
     uxdroid service disable <package> [--profile <name>] [--now]\n       \
     uxdroid service status <package> [--profile <name>]\n       \
     uxdroid service run <package> [--profile <name>] [--data-dir <dir>] [--runtime <dir>]\n";

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A home, a private runtime directory, a fake runtime and a fake `systemctl`.
    fn new(name: &str) -> Scratch {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("service-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["run", "home", "bin", "runtime/framework", "runtime/bin"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::set_permissions(dir.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.join("runtime/framework/framework.jar"), b"").unwrap();
        let systemctl = dir.join("bin/systemctl");
        std::fs::write(
            &systemctl,
            format!(
                "#!/bin/sh\necho \"$*\" >> {}\nexit ${{FAKE_SYSTEMCTL_EXIT:-0}}\n",
                dir.join("systemctl.log").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        Scratch { dir }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.join(sub)
    }

    /// Installs a sample APK into `data`, as `uxdroid install` leaves it, asking for `permissions`.
    fn install(&self, data: &Path, permissions: &[&'static str]) {
        let mut manifest = sample_manifest();
        for permission in permissions {
            manifest = manifest
                .start(
                    "uses-permission",
                    &[android("name", ATTR_NAME, Value::Str(permission))],
                )
                .end("uses-permission");
        }
        let code = data.join("app/com.example.app");
        std::fs::create_dir_all(&code).unwrap();
        Apk::new().manifest(&manifest).write(&code.join("base.apk"));
    }

    fn uxdroid(&self, args: &[&str]) -> Output {
        self.uxdroid_with(args, &[])
    }

    fn uxdroid_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_uxdroid"))
            .args(args)
            .env("PATH", path)
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("HOME", self.path("home"))
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("UXDROID_RUNTIME", self.path("runtime"))
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn units(&self) -> PathBuf {
        self.path("home/.config/systemd/user")
    }

    fn systemctl_calls(&self) -> String {
        std::fs::read_to_string(self.path("systemctl.log")).unwrap_or_default()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn exe() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_uxdroid")).unwrap()
}

#[test]
fn enable_writes_a_socket_and_a_service_unit() {
    let scratch = Scratch::new("enable");
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let units = scratch.units();
    let socket = units.join("uxdroid-com.example.app@default.socket");
    let service = units.join("uxdroid-com.example.app@default.service");
    assert_eq!(
        stdout(&output),
        format!(
            "wrote {}\nwrote {}\n\
             to start com.example.app on demand, run: systemctl --user daemon-reload && systemctl --user enable --now uxdroid-com.example.app@default.socket\n",
            socket.display(),
            service.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&socket).unwrap(),
        "# Written by uxdroid service enable for com.example.app, profile default.\n\
         # uxdroid service disable com.example.app removes it.\n\
         [Unit]\n\
         Description=Control socket of uxdroid app com.example.app, profile default\n\
         \n\
         [Socket]\n\
         ListenStream=%t/uxdroid/com.example.app.sock\n\
         SocketMode=0600\n\
         DirectoryMode=0700\n\
         FileDescriptorName=control\n\
         Service=uxdroid-com.example.app@default.service\n\
         RemoveOnStop=yes\n\
         \n\
         [Install]\n\
         WantedBy=sockets.target\n"
    );
    let runtime = std::fs::canonicalize(scratch.path("runtime")).unwrap();
    let data = std::fs::canonicalize(&data).unwrap();
    assert_eq!(
        std::fs::read_to_string(&service).unwrap(),
        format!(
            "# Written by uxdroid service enable for com.example.app, profile default.\n\
             # uxdroid service disable com.example.app removes it.\n\
             [Unit]\n\
             Description=uxdroid app com.example.app, profile default\n\
             Requires=uxdroid-com.example.app@default.socket\n\
             After=uxdroid-com.example.app@default.socket graphical-session.target\n\
             StartLimitIntervalSec=60\n\
             StartLimitBurst=5\n\
             \n\
             [Service]\n\
             Type=exec\n\
             ExecStart={exe} service run com.example.app --profile default --data-dir {data} --runtime {runtime}\n\
             Environment=UXDROID_PROFILE=default\n\
             Environment=UXDROID_RUNTIME={runtime}\n\
             Environment=ANDROID_DATA={data}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             RestartPreventExitStatus=75\n\
             TimeoutStopSec=15\n\
             StandardOutput=journal\n\
             StandardError=journal\n\
             SyslogIdentifier=uxdroid-com.example.app@default\n\
             CPUWeight=100\n\
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
             RestrictAddressFamilies=AF_UNIX AF_NETLINK\n",
            exe = exe().display(),
            data = data.display(),
            runtime = runtime.display(),
        )
    );
    for unit in [&socket, &service] {
        let mode = std::fs::metadata(unit).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "{}", unit.display());
    }
    assert_eq!(
        scratch.systemctl_calls(),
        "",
        "systemctl runs only with --now"
    );
}

#[test]
fn options_set_the_profile_limits_network_and_login_start() {
    let scratch = Scratch::new("options");
    let data = scratch.path("home/.local/share/uxdroid/profiles/work");
    scratch.install(&data, &["android.permission.INTERNET"]);
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--profile",
        "work",
        "--memory-max",
        "1G",
        "--memory-high",
        "800M",
        "--cpu-weight",
        "50",
        "--on-login",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with(
            "to start com.example.app on demand, run: systemctl --user daemon-reload && systemctl --user enable --now uxdroid-com.example.app@work.socket && systemctl --user enable uxdroid-com.example.app@work.service\n"
        ),
        "{}",
        stdout(&output)
    );
    let socket =
        std::fs::read_to_string(scratch.units().join("uxdroid-com.example.app@work.socket"))
            .unwrap();
    assert!(socket.contains("\nListenStream=%t/uxdroid/com.example.app@work.sock\n"));
    let service =
        std::fs::read_to_string(scratch.units().join("uxdroid-com.example.app@work.service"))
            .unwrap();
    let data = std::fs::canonicalize(&data).unwrap();
    for line in [
        format!("--profile work --data-dir {} --runtime", data.display()),
        "\nEnvironment=UXDROID_PROFILE=work\n".into(),
        "\nMemoryHigh=800M\nMemoryMax=1G\nCPUWeight=50\n".into(),
        "\nRestrictAddressFamilies=AF_UNIX AF_NETLINK AF_INET AF_INET6\n".into(),
        "\n[Install]\nWantedBy=default.target\n".into(),
    ] {
        assert!(service.contains(&line), "{line:?} in\n{service}");
    }
}

#[test]
fn now_runs_systemctl_for_the_user() {
    let scratch = Scratch::new("now");
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        data.to_str().unwrap(),
        "--on-login",
        "--now",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        scratch.systemctl_calls(),
        "--user daemon-reload\n\
         --user enable --now uxdroid-com.example.app@default.socket\n\
         --user enable uxdroid-com.example.app@default.service\n"
    );
    assert!(
        stdout(&output).ends_with(
            "ran: systemctl --user daemon-reload\n\
             ran: systemctl --user enable --now uxdroid-com.example.app@default.socket\n\
             ran: systemctl --user enable uxdroid-com.example.app@default.service\n"
        ),
        "{}",
        stdout(&output)
    );
    std::fs::remove_file(scratch.path("systemctl.log")).unwrap();
    let output = scratch.uxdroid(&["service", "disable", "com.example.app", "--now"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        scratch.systemctl_calls(),
        "--user disable --now uxdroid-com.example.app@default.socket uxdroid-com.example.app@default.service\n\
         --user daemon-reload\n"
    );
    assert!(
        !scratch
            .units()
            .join("uxdroid-com.example.app@default.socket")
            .exists()
    );
    assert!(
        !scratch
            .units()
            .join("uxdroid-com.example.app@default.service")
            .exists()
    );
    // A failing systemctl is reported, and the units it was given stay.
    let output = scratch.uxdroid_with(
        &[
            "service",
            "enable",
            "com.example.app",
            "--data-dir",
            data.to_str().unwrap(),
            "--now",
        ],
        &[("FAKE_SYSTEMCTL_EXIT", "1")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: systemctl --user daemon-reload failed with exit status: 1\n"
    );
    assert!(
        scratch
            .units()
            .join("uxdroid-com.example.app@default.socket")
            .exists()
    );
}

#[test]
fn disable_removes_only_its_own_units_and_their_links() {
    let scratch = Scratch::new("disable");
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let output = scratch.uxdroid(&["service", "disable", "com.example.app"]);
    assert_eq!(
        (output.status.code(), stdout(&output)),
        (
            Some(0),
            "com.example.app has no service for profile default\n".to_string()
        )
    );
    let enabled = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        data.to_str().unwrap(),
        "--on-login",
    ]);
    assert_eq!(enabled.status.code(), Some(0), "{}", stderr(&enabled));
    // What `systemctl --user enable` links.
    let units = scratch.units();
    for (wants, unit) in [
        (
            "sockets.target.wants",
            "uxdroid-com.example.app@default.socket",
        ),
        (
            "default.target.wants",
            "uxdroid-com.example.app@default.service",
        ),
    ] {
        std::fs::create_dir_all(units.join(wants)).unwrap();
        std::os::unix::fs::symlink(units.join(unit), units.join(wants).join(unit)).unwrap();
    }
    let output = scratch.uxdroid(&["service", "disable", "com.example.app"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "removed {units}/uxdroid-com.example.app@default.socket\n\
             removed {units}/uxdroid-com.example.app@default.service\n\
             to stop what still runs, run: systemctl --user stop uxdroid-com.example.app@default.socket uxdroid-com.example.app@default.service && systemctl --user daemon-reload\n",
            units = units.display()
        )
    );
    for path in [
        "uxdroid-com.example.app@default.socket",
        "uxdroid-com.example.app@default.service",
        "sockets.target.wants/uxdroid-com.example.app@default.socket",
        "default.target.wants/uxdroid-com.example.app@default.service",
    ] {
        assert!(
            std::fs::symlink_metadata(units.join(path)).is_err(),
            "{path}"
        );
    }
    // A unit of that name that uxdroid did not write is the user's own.
    std::fs::write(
        units.join("uxdroid-com.example.app@default.service"),
        "[Service]\n",
    )
    .unwrap();
    let output = scratch.uxdroid(&["service", "disable", "com.example.app"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "error: {} was not written by uxdroid service enable; it is left as it is\n",
            units
                .join("uxdroid-com.example.app@default.service")
                .display()
        )
    );
    assert!(
        units
            .join("uxdroid-com.example.app@default.service")
            .exists()
    );
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output)
            .ends_with("was not written by uxdroid service enable; it is left as it is\n")
    );
}

#[test]
fn status_reports_the_units_and_whether_the_app_runs() {
    let scratch = Scratch::new("status");
    let output = scratch.uxdroid(&["service", "status", "com.example.app", "--profile", "work"]);
    assert_eq!(
        (output.status.code(), stdout(&output)),
        (
            Some(3),
            "com.example.app has no service for profile work\n".to_string()
        )
    );
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let enabled = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--profile",
        "work",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(enabled.status.code(), Some(0), "{}", stderr(&enabled));
    // The socket unit's socket, listening before any start: status must not connect, which would start
    // the app.
    let control = scratch.path("run/uxdroid");
    std::fs::create_dir_all(&control).unwrap();
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manager =
        std::os::unix::net::UnixListener::bind(control.join("com.example.app@work.sock")).unwrap();
    manager.set_nonblocking(true).unwrap();
    let output = scratch.uxdroid(&["service", "status", "com.example.app", "--profile", "work"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "service: uxdroid-com.example.app@work.service\n\
             socket unit: uxdroid-com.example.app@work.socket\n\
             units in: {}\n\
             socket: {}\n\
             data: {}\n\
             on login: no\n\
             running: no\n",
            scratch.units().display(),
            control.join("com.example.app@work.sock").display(),
            std::fs::canonicalize(&data).unwrap().display()
        )
    );
    assert!(manager.accept().is_err(), "status connected to the socket");
    assert_eq!(scratch.systemctl_calls(), "");
}

#[test]
fn run_starts_the_app_in_the_foreground_with_its_profile() {
    let scratch = Scratch::new("run");
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let dalvikvm = scratch.path("runtime/bin/dalvikvm64");
    std::fs::write(
        &dalvikvm,
        "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done\necho \"UXDROID_PROFILE=$UXDROID_PROFILE\"\necho \"ANDROID_DATA=$ANDROID_DATA\"\necho \"LISTEN_PID=$LISTEN_PID\"\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&dalvikvm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = scratch.uxdroid_with(
        &[
            "service",
            "run",
            "com.example.app",
            "--profile",
            "work",
            "--data-dir",
            data.to_str().unwrap(),
        ],
        &[("LISTEN_PID", "1234")],
    );
    // The launcher replaced the command, so its status is the app's.
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    let lines = stdout(&output);
    let lines: Vec<&str> = lines.lines().collect();
    let launcher = lines
        .iter()
        .position(|line| *line == "dev.uxdroid.system.Launcher")
        .unwrap_or_else(|| panic!("{lines:?}"));
    let data = std::fs::canonicalize(&data).unwrap();
    assert_eq!(
        &lines[launcher + 3..],
        &[
            &format!("{}/app/com.example.app/base.apk", data.display())[..],
            "UXDROID_PROFILE=work",
            &format!("ANDROID_DATA={}", data.display()),
            // What systemd passes reaches the launcher, which checks it.
            "LISTEN_PID=1234",
        ][..]
    );
}

#[test]
fn bad_input_is_refused() {
    let scratch = Scratch::new("refused");
    let data = scratch.path("data");
    scratch.install(&data, &[]);
    let dir = data.to_str().unwrap();
    for args in [
        &["service"][..],
        &["service", "enable"],
        &["service", "restart", "com.example.app"],
        &["service", "enable", "com.example.app", "--now", "--now"],
        &["service", "enable", "com.example.app", "--bogus"],
        &[
            "service",
            "enable",
            "com.example.app",
            "--memory-max",
            "lots",
        ],
        &["service", "enable", "com.example.app", "--memory-max", "1Q"],
        &[
            "service",
            "enable",
            "com.example.app",
            "--memory-high",
            "101%",
        ],
        &["service", "enable", "com.example.app", "--cpu-weight", "0"],
        &[
            "service",
            "enable",
            "com.example.app",
            "--cpu-weight",
            "10001",
        ],
        &[
            "service",
            "enable",
            "com.example.app",
            "--data-dir",
            "relative",
        ],
        &["service", "disable", "com.example.app", "--on-login"],
        &["service", "status", "com.example.app", "--now"],
        &["service", "run", "com.example.app", "--now"],
    ] {
        let output = scratch.uxdroid(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).ends_with(SERVICE_USAGE),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    for (args, message) in [
        (
            &["service", "enable", "../evil"][..],
            "error: \"../evil\" is not a package name\n",
        ),
        (
            &["service", "enable", "com.example.app\n[Service]"],
            "error: \"com.example.app\\n[Service]\" is not a package name\n",
        ),
        (
            &["service", "status", "com.example.app", "--profile", "a-b"],
            "error: \"a-b\" is not a profile name\n",
        ),
    ] {
        let output = scratch.uxdroid(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(stderr(&output), message, "{args:?}");
    }
    let output = scratch.uxdroid(&["service", "enable", "com.other", "--data-dir", dir]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "error: com.other is not installed in {dir}: run uxdroid install <apk> --data {dir}\n"
        )
    );
    // A path a unit file cannot hold as one word.
    let spaced = scratch.path("data with space");
    scratch.install(&spaced, &[]);
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        spaced.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "error: {} holds a space, a quote, a backslash or a control character, which a unit file cannot hold here\n",
            spaced.display()
        )
    );
    assert!(!scratch.units().exists(), "a refused enable wrote units");
}

#[test]
fn specifiers_in_paths_are_escaped() {
    let scratch = Scratch::new("escaped");
    let data = scratch.path("data%h$HOME");
    scratch.install(&data, &[]);
    let output = scratch.uxdroid(&[
        "service",
        "enable",
        "com.example.app",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let service = std::fs::read_to_string(
        scratch
            .units()
            .join("uxdroid-com.example.app@default.service"),
    )
    .unwrap();
    let data = std::fs::canonicalize(&data).unwrap();
    let raw = data.display().to_string();
    let escaped = raw.replace('%', "%%");
    assert!(
        service.contains(&format!(
            "--data-dir {} --runtime",
            escaped.replace('$', "$$")
        )),
        "{service}"
    );
    assert!(service.contains(&format!("\nEnvironment=ANDROID_DATA={escaped}\n")));
    assert!(service.contains(&format!("\nReadWritePaths={escaped} %t/uxdroid\n")));
    let output = scratch.uxdroid(&["service", "status", "com.example.app"]);
    assert!(
        stdout(&output).contains(&format!("\ndata: {raw}\n")),
        "{}",
        stdout(&output)
    );
}
