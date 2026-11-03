//! `uxdroid app <package> ui|screenshot|tap|type|scroll|back|key|wait`: agent commands to a running
//! app. The app is played by a control server of `uxdroid_system::control` with a scripted handler;
//! real apps are covered by runtime/framework/agent_smoke.sh.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uxdroid_system::control::png;
use uxdroid_system::control::protocol::{
    Command as Request, Condition, Failure, Place, Screenshot, Scroll, Typing, Ui, Wait,
};
use uxdroid_system::control::server::{self, Bound, Handler, Info, Server};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("agent-{name}"));
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
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A tiny red-and-blue image, as the app's screenshot.
fn image() -> Vec<u8> {
    png::encode(&png::Image {
        width: 2,
        height: 1,
        rgb: vec![255, 0, 0, 0, 0, 255],
    })
    .expect("the pixels fill the image")
}

/// What a running app with an agent answers; records what it is sent.
#[derive(Default)]
struct App {
    received: Mutex<Vec<Request>>,
    refuse: Option<Failure>,
}

impl Handler for App {
    fn handle(&self, command: Request) -> Result<Value, Failure> {
        self.received.lock().unwrap().push(command.clone());
        if let Some(failure) = &self.refuse {
            return Err(failure.clone());
        }
        Ok(match command {
            Request::Ui(_) => {
                json!({"snapshot": 3, "windows": [{"root": {"id": 1, "text": "Tap\nme"}}]})
            }
            Request::Screenshot(_) => json!({
                "width": 2, "height": 1, "scale": 1, "windows": 1,
                "png": png::base64_encode(&image()),
            }),
            Request::Tap(Place::Node { node, .. }) => json!({"x": 5, "y": 6, "node": node}),
            Request::Tap(Place::Point { x, y })
            | Request::Scroll(Scroll {
                at: Place::Point { x, y },
                ..
            }) => {
                json!({"x": x, "y": y})
            }
            Request::Scroll(_) => json!({"x": 7, "y": 8}),
            Request::Type(typing) => {
                json!({"typed": typing.text.chars().count(), "keys": 2, "committed": 1})
            }
            Request::Back => json!({"key": "BACK"}),
            Request::Key(name) => json!({"key": name}),
            Request::Wait(Wait {
                condition: Condition::Idle,
                ..
            }) => json!({"idle": true, "waited_ms": 512}),
            Request::Wait(_) => {
                json!({"snapshot": 4, "node": {"id": 2, "text": "Done"}, "waited_ms": 80})
            }
            other => json!({"unexpected": other.name()}),
        })
    }
}

fn serve(dir: &Path, app: App) -> (Server, Arc<App>) {
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let Bound::Listening(listener) =
        server::bind_instance(&control, "com.example.app", "default").unwrap()
    else {
        panic!("the socket is taken");
    };
    let app = Arc::new(app);
    let info = Info {
        package: "com.example.app".into(),
        pid: 99,
    };
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    (
        server::serve(listener, info, uid, app.clone()).unwrap(),
        app,
    )
}

fn run(dir: &Path, args: &[&str]) -> Output {
    let mut all = vec!["app", "com.example.app"];
    all.extend_from_slice(args);
    uxdroid(dir, &all)
}

#[test]
fn ui_prints_the_snapshot_as_one_line_of_json() {
    let dir = scratch("ui");
    let (server, app) = serve(&dir, App::default());
    let output = run(&dir, &["ui", "--max-nodes", "50", "--max-depth", "8"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed = stdout(&output);
    assert_eq!(printed.lines().count(), 1, "{printed}");
    let snapshot: Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(snapshot["windows"][0]["root"]["text"], json!("Tap\nme"));
    let output = run(&dir, &["ui"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        *app.received.lock().unwrap(),
        vec![
            Request::Ui(Ui {
                max_nodes: 50,
                max_depth: 8
            }),
            Request::Ui(Ui {
                max_nodes: 1000,
                max_depth: 64
            }),
        ]
    );
    server.shutdown();
}

#[test]
fn screenshot_writes_the_png_the_app_sent() {
    let dir = scratch("screenshot");
    let (server, app) = serve(&dir, App::default());
    let file = dir.join("shot.png");
    let path = file.to_str().unwrap();
    let output = run(
        &dir,
        &["screenshot", path, "--window", "1", "--max-size", "640"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("wrote {path} (2x1)\n"));
    assert_eq!(std::fs::read(&file).unwrap(), image());
    assert_eq!(
        app.received.lock().unwrap()[0],
        Request::Screenshot(Screenshot {
            window: Some(1),
            max_size: 640
        })
    );
    server.shutdown();
}

#[test]
fn a_screenshot_that_is_not_a_png_is_refused() {
    struct Broken;
    impl Handler for Broken {
        fn handle(&self, _: Request) -> Result<Value, Failure> {
            Ok(json!({"width": 1, "height": 1, "png": "not base64!"}))
        }
    }
    let dir = scratch("broken");
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let Bound::Listening(listener) = server::bind(&control, "com.example.app").unwrap() else {
        panic!("taken");
    };
    let info = Info {
        package: "com.example.app".into(),
        pid: 1,
    };
    // SAFETY: geteuid has no preconditions.
    let server =
        server::serve(listener, info, unsafe { libc::geteuid() }, Arc::new(Broken)).unwrap();
    let file = dir.join("shot.png");
    let output = run(&dir, &["screenshot", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
    assert!(!file.exists());
    server.shutdown();
}

#[test]
fn actions_reach_the_app_and_report_what_they_did() {
    let dir = scratch("actions");
    let (server, app) = serve(&dir, App::default());
    for (args, printed) in [
        (&["tap", "10", "20"][..], "tapped 10,20\n"),
        (
            &["tap", "--node", "4", "--snapshot", "3"],
            "tapped node 4 at 5,6\n",
        ),
        (
            &["type", "hé\nllo"],
            "typed 6 characters: 2 keys, 1 committed\n",
        ),
        (
            &["type", "x", "--node", "4"],
            "typed 1 characters: 2 keys, 1 committed\n",
        ),
        (&["scroll", "1", "2", "--dy", "3"], "scrolled at 1,2\n"),
        (
            &["scroll", "--node", "9", "--dx", "-2"],
            "scrolled at 7,8\n",
        ),
        (&["back"], "pressed BACK\n"),
        (&["key", "ENTER"], "pressed ENTER\n"),
        (
            &["wait", "--idle", "--timeout", "2000"],
            "idle after 512 ms\n",
        ),
    ] {
        let output = run(&dir, args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(stdout(&output), printed, "{args:?}");
    }
    let output = run(&dir, &["wait", "--text", "Done"]);
    assert_eq!(output.status.code(), Some(0));
    let found: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(found["node"]["id"], json!(2));
    let node = |node, snapshot| Place::Node { node, snapshot };
    assert_eq!(
        *app.received.lock().unwrap(),
        vec![
            Request::Tap(Place::Point { x: 10, y: 20 }),
            Request::Tap(node(4, Some(3))),
            Request::Type(Typing {
                text: "hé\nllo".into(),
                node: None
            }),
            Request::Type(Typing {
                text: "x".into(),
                node: Some(node(4, None))
            }),
            Request::Scroll(Scroll {
                at: Place::Point { x: 1, y: 2 },
                dx: 0,
                dy: 3
            }),
            Request::Scroll(Scroll {
                at: node(9, None),
                dx: -2,
                dy: 0
            }),
            Request::Back,
            Request::Key("ENTER".into()),
            Request::Wait(Wait {
                condition: Condition::Idle,
                timeout_ms: 2000
            }),
            Request::Wait(Wait {
                condition: Condition::Text("Done".into()),
                timeout_ms: 10000
            }),
        ]
    );
    server.shutdown();
}

#[test]
fn a_refusal_exits_1_with_its_code() {
    let dir = scratch("refused");
    let refusing = App {
        refuse: Some(Failure::new(
            "timeout",
            "the condition did not hold within 300 ms",
        )),
        ..App::default()
    };
    let (server, _) = serve(&dir, refusing);
    let output = run(&dir, &["wait", "--text", "never", "--timeout", "300"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "error: com.example.app refused: the condition did not hold within 300 ms (timeout)\n"
    );
    server.shutdown();
}

#[test]
fn agent_commands_to_an_app_that_is_not_running_exit_3() {
    let dir = scratch("not-running");
    for args in [
        &["ui"][..],
        &["screenshot", "/tmp/x.png"],
        &["tap", "1", "1"],
        &["type", "a"],
        &["scroll", "1", "1", "--dy", "1"],
        &["back"],
        &["key", "ENTER"],
        &["wait", "--idle"],
    ] {
        let output = run(&dir, args);
        assert_eq!(output.status.code(), Some(3), "{args:?}");
        assert_eq!(
            stdout(&output),
            "com.example.app is not running\n",
            "{args:?}"
        );
    }
}

#[test]
fn wrong_agent_usage_exits_2() {
    let dir = scratch("usage");
    for args in [
        &["ui", "extra"][..],
        &["ui", "--max-nodes", "0"],
        &["screenshot"],
        &["screenshot", "a.png", "--max-size", "1"],
        &["tap"],
        &["tap", "1"],
        &["tap", "1", "2", "--node", "3"],
        &["tap", "-1", "2"],
        &["type"],
        &["type", ""],
        &["type", "a", "b"],
        &["scroll", "1", "2"],
        &["scroll", "1", "2", "--dy", "99"],
        &["back", "now"],
        &["key"],
        &["key", "enter"],
        &["wait"],
        &["wait", "--text", "a", "--idle"],
        &["wait", "--idle", "--timeout", "0"],
    ] {
        let output = run(&dir, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(stderr(&output).contains("usage: uxdroid app"), "{args:?}");
    }
}
