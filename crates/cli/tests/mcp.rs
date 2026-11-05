//! `uxdroid mcp`: an MCP server over stdio whose tools are the control commands of running apps. The
//! app is played by a control server of `uxdroid_system::control` with a scripted handler.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uxdroid_system::control::png;
use uxdroid_system::control::protocol::{Command as Request, Failure, Place, Ui};
use uxdroid_system::control::server::{self, Bound, Handler, Info, Server, Session};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("run")).unwrap();
    std::fs::create_dir_all(path.join("home")).unwrap();
    std::fs::set_permissions(path.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[derive(Default)]
struct App {
    received: Mutex<Vec<Request>>,
    ended: AtomicUsize,
}

impl Handler for App {
    fn handle(&self, command: Request) -> Result<Value, Failure> {
        self.received.lock().unwrap().push(command.clone());
        match command {
            Request::Status => {
                Ok(json!({"package": "com.example.app", "pid": 7, "data": "/d", "activities": []}))
            }
            Request::Ui(_) => Ok(json!({"snapshot": 1, "windows": []})),
            Request::Screenshot(_) => Ok(json!({
                "width": 1, "height": 1, "scale": 1, "windows": 1,
                "png": png::base64_encode(&png::encode(&png::Image { width: 1, height: 1, rgb: vec![1, 2, 3] }).unwrap()),
            })),
            Request::Tap(Place::Point { x, y }) => Ok(json!({"x": x, "y": y})),
            Request::Wait(_) => Err(Failure::new(
                "timeout",
                "the condition did not hold within 5 ms",
            )),
            other => Ok(json!({"done": other.name()})),
        }
    }

    fn ended(&self, _: &Session) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}

fn serve(dir: &Path) -> (Server, Arc<App>) {
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let Bound::Listening(listener) = server::bind(&control, "com.example.app").unwrap() else {
        panic!("the socket is taken");
    };
    let app = Arc::new(App::default());
    let info = Info {
        package: "com.example.app".into(),
        pid: 7,
    };
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    (
        server::serve(listener, info, uid, app.clone()).unwrap(),
        app,
    )
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Mcp {
    fn start(dir: &Path) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_uxdroid"))
            .arg("mcp")
            .env("XDG_RUNTIME_DIR", dir.join("run"))
            .env("HOME", dir.join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.input, "{line}").unwrap();
        self.input.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        assert!(
            line.ends_with('\n') && !line[..line.len() - 1].contains('\n'),
            "{line:?}"
        );
        serde_json::from_str(&line).unwrap()
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        let answer = self.read();
        assert_eq!(answer["jsonrpc"], json!("2.0"));
        assert_eq!(answer["id"], json!(id));
        answer
    }

    fn tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.call(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )["result"]
            .clone()
    }

    /// Closes stdin, which ends the server.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.input);
        self.child.wait().unwrap()
    }
}

fn percent_decoded(text: &str) -> String {
    let mut bytes = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'%' {
            let hex = std::str::from_utf8(&tail[..2]).unwrap();
            bytes.push(u8::from_str_radix(hex, 16).unwrap());
            rest = &tail[2..];
        } else {
            bytes.push(first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn initialize_negotiates_the_version_and_names_the_tools_capability() {
    let dir = scratch("initialize");
    let mut mcp = Mcp::start(&dir);
    let answer = mcp.call(1, "initialize", json!({
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}
    }));
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], json!("2025-06-18"));
    assert_eq!(result["serverInfo"]["name"], json!("uxdroid"));
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["instructions"].as_str().unwrap().contains("ui"));
    // The client may show the server with the project's icon.
    let icon = &result["serverInfo"]["icons"][0];
    assert_eq!(icon["mimeType"], json!("image/svg+xml"));
    assert_eq!(icon["sizes"], json!(["any"]));
    let src = icon["src"].as_str().unwrap();
    let encoded = src.strip_prefix("data:image/svg+xml,").unwrap();
    // A URL parser ends the data at '#' and refuses spaces, so both are percent-encoded.
    assert!(!encoded.contains(['#', ' ']), "{src}");
    let logo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/logo/icon.svg");
    assert_eq!(
        percent_decoded(encoded),
        std::fs::read_to_string(logo).unwrap()
    );
    // A notification is never answered: the next answer is the ping's.
    mcp.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!(mcp.call(2, "ping", json!({}))["result"], json!({}));
    // A version this server does not know gets its latest.
    let answer = mcp.call(
        3,
        "initialize",
        json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], json!("2025-11-25"));
    assert!(mcp.finish().success());
}

#[test]
fn discover_lists_the_supported_versions_and_unknown_ones_are_refused() {
    let dir = scratch("discover");
    let mut mcp = Mcp::start(&dir);
    let answer = mcp.call(
        1,
        "server/discover",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    );
    let result = &answer["result"];
    assert_eq!(result["resultType"], json!("complete"));
    assert!(
        result["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        json!("uxdroid")
    );
    let answer = mcp.call(
        2,
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01"}}),
    );
    assert_eq!(answer["error"]["code"], json!(-32022));
    assert_eq!(answer["error"]["data"]["requested"], json!("1900-01-01"));
    assert!(mcp.finish().success());
}

#[test]
fn tools_map_one_to_one_onto_the_control_commands() {
    let dir = scratch("list");
    let mut mcp = Mcp::start(&dir);
    let tools = mcp.call(1, "tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "list",
            "status",
            "start",
            "stop",
            "intent",
            "ui",
            "screenshot",
            "tap",
            "type",
            "scroll",
            "back",
            "key",
            "wait"
        ]
    );
    for tool in tools.as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "{tool}");
        assert!(!tool["description"].as_str().unwrap().is_empty());
        if tool["name"] != json!("list") {
            assert!(
                tool["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("package")),
                "{tool}"
            );
        }
    }
    assert!(mcp.finish().success());
}

#[test]
fn tool_calls_reach_the_app_over_one_connection() {
    let dir = scratch("call");
    let (server, app) = serve(&dir);
    let mut mcp = Mcp::start(&dir);
    let result = mcp.tool(
        1,
        "ui",
        json!({"package": "com.example.app", "max_nodes": 5}),
    );
    assert_eq!(result["isError"], json!(false));
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["snapshot"], json!(1));
    let result = mcp.tool(
        2,
        "tap",
        json!({"package": "com.example.app", "x": 3, "y": 4}),
    );
    assert_eq!(result["content"][0]["text"], json!("tapped 3,4"));
    let result = mcp.tool(3, "screenshot", json!({"package": "com.example.app"}));
    let image = &result["content"][0];
    assert_eq!(image["type"], json!("image"));
    assert_eq!(image["mimeType"], json!("image/png"));
    let data = png::base64_decode(image["data"].as_str().unwrap()).unwrap();
    assert_eq!(png::decode(&data).unwrap().rgb, vec![1, 2, 3]);
    // The agent commands share one connection, so accessibility stays on between calls.
    assert_eq!(app.ended.load(Ordering::SeqCst), 0);
    let result = mcp.tool(4, "status", json!({"package": "com.example.app"}));
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("package: com.example.app\n")
    );
    assert_eq!(
        *app.received.lock().unwrap(),
        vec![
            Request::Ui(Ui {
                max_nodes: 5,
                max_depth: 64
            }),
            Request::Tap(Place::Point { x: 3, y: 4 }),
            Request::Screenshot(uxdroid_system::control::protocol::Screenshot {
                window: None,
                max_size: 2048
            }),
            Request::Status,
        ]
    );
    // status asks over a connection of its own, as uxdroid app status does.
    assert_eq!(app.ended.load(Ordering::SeqCst), 1);
    assert!(mcp.finish().success());
    for _ in 0..100 {
        if app.ended.load(Ordering::SeqCst) == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.ended.load(Ordering::SeqCst), 2);
    server.shutdown();
}

#[test]
fn bad_arguments_and_refusals_are_tool_errors_the_agent_can_read() {
    let dir = scratch("errors");
    let (server, app) = serve(&dir);
    let mut mcp = Mcp::start(&dir);
    for (arguments, says) in [
        (
            json!({"package": "com.example.app", "x": 3}),
            "tap takes a node",
        ),
        (
            json!({"package": "com.example.app", "x": -3, "y": 1}),
            "from 0 to 16383",
        ),
        (
            json!({"package": "com.example.app", "x": 1, "y": 1, "cmd": "stop"}),
            "unknown argument cmd",
        ),
        (
            json!({"package": "../evil", "x": 1, "y": 1}),
            "not a package name",
        ),
        (json!({"x": 1, "y": 1}), "package"),
    ] {
        let result = mcp.tool(1, "tap", arguments.clone());
        assert_eq!(result["isError"], json!(true), "{arguments}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(says), "{arguments}: {text}");
    }
    let result = mcp.tool(
        2,
        "wait",
        json!({"package": "com.example.app", "text": "x", "timeout_ms": 5}),
    );
    assert_eq!(result["isError"], json!(true));
    assert_eq!(
        result["content"][0]["text"],
        json!("com.example.app refused: the condition did not hold within 5 ms (timeout)")
    );
    let result = mcp.tool(3, "ui", json!({"package": "org.not.running"}));
    assert_eq!(result["isError"], json!(true));
    assert_eq!(
        result["content"][0]["text"],
        json!("org.not.running is not running")
    );
    // Nothing invalid reached the app.
    assert_eq!(app.received.lock().unwrap().len(), 1);
    assert!(mcp.finish().success());
    server.shutdown();
}

#[test]
fn protocol_errors_are_json_rpc_errors() {
    let dir = scratch("protocol");
    let mut mcp = Mcp::start(&dir);
    assert_eq!(
        mcp.call(1, "nope", json!({}))["error"]["code"],
        json!(-32601)
    );
    assert_eq!(
        mcp.call(
            2,
            "tools/call",
            json!({"name": "teleport", "arguments": {}})
        )["error"]["code"],
        json!(-32602)
    );
    assert_eq!(
        mcp.call(3, "tools/call", json!({"arguments": {}}))["error"]["code"],
        json!(-32602)
    );
    mcp.send("{not json");
    let answer = mcp.read();
    assert_eq!(
        (answer["id"].clone(), answer["error"]["code"].clone()),
        (Value::Null, json!(-32700))
    );
    mcp.send(r#"[{"jsonrpc":"2.0","id":4,"method":"ping"}]"#);
    assert_eq!(mcp.read()["error"]["code"], json!(-32600));
    mcp.send(r#"{"jsonrpc":"1.0","id":5,"method":"ping"}"#);
    assert_eq!(mcp.read()["error"]["code"], json!(-32600));
    // A line longer than the limit is refused, and the next one is read.
    mcp.send(&format!(
        r#"{{"jsonrpc":"2.0","id":6,"method":"ping","params":{{"x":"{}"}}}}"#,
        "a".repeat(2 << 20)
    ));
    assert_eq!(mcp.read()["error"]["code"], json!(-32600));
    assert_eq!(mcp.call(7, "ping", json!({}))["result"], json!({}));
    assert!(mcp.finish().success());
}

/// What one connection of a scripted app does with the request it reads.
#[derive(Clone, Copy)]
enum Turn {
    /// Answers it, then closes the connection, as an app closing an idle one.
    AnswerAndClose,
    /// Closes without answering, as an app that ended while running it.
    Drop,
    /// Answers it and every later request.
    Answer,
}

/// An app on a raw socket that holds its instance lock and plays `turns`, one per connection.
fn scripted(dir: &Path, turns: Vec<Turn>) -> Arc<Mutex<Vec<String>>> {
    let control = uxdroid_system::control::control_dir_in(&dir.join("run")).unwrap();
    let lock = uxdroid_system::control::claim_instance(&control.join("com.example.app.lock"))
        .unwrap()
        .unwrap();
    let listener =
        std::os::unix::net::UnixListener::bind(control.join("com.example.app.sock")).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    std::thread::spawn(move || {
        let _lock = lock;
        for turn in turns {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let request: Value = serde_json::from_str(&line).unwrap();
                let command = request["cmd"].as_str().unwrap().to_string();
                record.lock().unwrap().push(command.clone());
                if matches!(turn, Turn::Drop) {
                    break;
                }
                let result = match command.as_str() {
                    "tap" => json!({"x": 3, "y": 4}),
                    _ => json!({"snapshot": 1, "windows": []}),
                };
                let answer = json!({"v": 1, "id": request["id"], "ok": true, "result": result});
                writeln!(writer, "{answer}").unwrap();
                if matches!(turn, Turn::AnswerAndClose) {
                    break;
                }
            }
        }
    });
    seen
}

#[test]
fn a_connection_the_app_closed_is_opened_again_without_repeating_a_call() {
    let dir = scratch("reconnect");
    let seen = scripted(&dir, vec![Turn::AnswerAndClose, Turn::Answer]);
    let mut mcp = Mcp::start(&dir);
    let ui = json!({"package": "com.example.app"});
    assert_eq!(mcp.tool(1, "ui", ui)["isError"], json!(false));
    std::thread::sleep(std::time::Duration::from_millis(50));
    let tap = json!({"package": "com.example.app", "x": 3, "y": 4});
    let result = mcp.tool(2, "tap", tap);
    assert_eq!(
        result["content"][0]["text"],
        json!("tapped 3,4"),
        "{result}"
    );
    assert_eq!(*seen.lock().unwrap(), ["ui", "tap"]);
    assert!(mcp.finish().success());
}

#[test]
fn a_call_that_may_have_acted_is_not_sent_again_and_a_reading_one_is() {
    let dir = scratch("no-replay");
    let seen = scripted(&dir, vec![Turn::Drop, Turn::Drop, Turn::Answer]);
    let mut mcp = Mcp::start(&dir);
    let tap = json!({"package": "com.example.app", "x": 3, "y": 4});
    let result = mcp.tool(1, "tap", tap);
    assert_eq!(result["isError"], json!(true), "{result}");
    assert_eq!(*seen.lock().unwrap(), ["tap"], "a tap is never repeated");
    // A ui snapshot reads only, so it is asked again on a new connection.
    let result = mcp.tool(2, "ui", json!({"package": "com.example.app"}));
    assert_eq!(result["isError"], json!(false), "{result}");
    assert_eq!(*seen.lock().unwrap(), ["tap", "ui", "ui"]);
    assert!(mcp.finish().success());
}
