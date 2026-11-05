//! `uxdroid mcp`: a Model Context Protocol server over stdio, so an agent such as Claude Code can
//! drive running apps. Its tools map one to one onto `uxdroid app` and the control protocol: each
//! takes the app's `package`, and an optional `profile`, besides the command's own arguments, which
//! the control protocol checks as it checks any request. One server serves every app, so a client
//! is set up once; it keeps one connection open per app, so the app's accessibility nodes stay on
//! between calls and turn off when the server ends.
//!
//! Both eras of MCP are spoken: `initialize` for versions up to 2025-11-25, and `server/discover`
//! with per-request `_meta` for 2026-07-28. Messages are one JSON-RPC 2.0 object per line.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Map, Value, json};
use uxdroid_system::control::client::{self, CallError};
use uxdroid_system::control::protocol::{self, Command as Request, Intent};
use uxdroid_system::control::{
    DEFAULT_PROFILE, control_dir, instance_socket, served, valid_package, valid_profile,
};

use crate::app::{self, Done, Instance};

/// Newest first. The last era that negotiates in `initialize` is 2025-11-25.
const VERSIONS: [&str; 5] = [
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];
const LATEST_INITIALIZE: &str = "2025-11-25";
const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
/// The longest message read; a screenshot goes the other way.
const MAX_MESSAGE: usize = 1 << 20;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "Drives Android apps that uxdroid runs. Start with list to see running \
apps, or start one. Call ui to read the screen as a tree of nodes with text, bounds and flags; \
tap, type and scroll take a node id from the last ui snapshot, or x and y on the display. Use wait \
instead of sleeping: for a text to show, or for the display to go idle. screenshot returns a PNG.";

fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn integer(description: &str, minimum: i64, maximum: i64) -> Value {
    json!({"type": "integer", "description": description, "minimum": minimum, "maximum": maximum})
}

/// The tools, in order, with their descriptions and own arguments.
fn tools() -> Vec<Value> {
    let node = || {
        integer(
            "A node id from the last ui snapshot; acts at its center.",
            1,
            u32::MAX as i64,
        )
    };
    let snapshot = || {
        integer(
            "The snapshot the node is from; refused once it is not the last.",
            0,
            i64::MAX,
        )
    };
    let x = || integer("Display x, with node absent.", 0, protocol::MAX_COORDINATE);
    let y = || integer("Display y, with node absent.", 0, protocol::MAX_COORDINATE);
    let intent = || {
        json!({
            "activity": string("An activity class, or .Name relative to the package."),
            "action": string("An intent action."),
            "data": string("A data URI."),
            "extras": {"type": "object", "additionalProperties": {"type": "string"}, "description": "String extras."},
            "flags": integer("Intent.setFlags.", 0, u32::MAX as i64),
        })
    };
    let mut start = intent();
    start["data_dir"] = string("The data directory to start from, absolute.");
    let tools: Vec<(&str, &str, Value, Vec<&str>)> = vec![
        (
            "list",
            "Lists running apps: instance, process and data directory, one a line.",
            json!({}),
            vec![],
        ),
        (
            "status",
            "The app's process and its activities, bottom to top.",
            json!({}),
            vec![],
        ),
        (
            "start",
            "Starts the installed app, or hands the intent to it when it runs.",
            start,
            vec![],
        ),
        (
            "stop",
            "Finishes every activity; the app's process exits.",
            json!({}),
            vec![],
        ),
        (
            "intent",
            "Sends an intent naming an activity, an action or data to the running app.",
            intent(),
            vec![],
        ),
        (
            "ui",
            "Reads the screen: each window's views and accessibility nodes as JSON, with id, class, \
             resource_id, text, description, hint, bounds [left, top, right, bottom] and flags \
             (absent means false). Ids hold for this snapshot.",
            json!({
                "max_nodes": integer("Most nodes reported.", 1, protocol::MAX_NODES as i64),
                "max_depth": integer("Deepest level reported.", 1, protocol::MAX_DEPTH as i64),
            }),
            vec![],
        ),
        (
            "screenshot",
            "The display as the compositor shows it, or one window, as a PNG image.",
            json!({
                "window": integer("One window, by its place in ui's windows, bottom first.", 0, protocol::MAX_WINDOWS as i64 - 1),
                "max_size": integer("Longest side of the image; larger displays are scaled down.", protocol::MIN_SHOT_SIZE as i64, protocol::MAX_SHOT_SIZE as i64),
            }),
            vec![],
        ),
        (
            "tap",
            "Taps a node, or a point, with a finger, as a user would.",
            json!({"node": node(), "snapshot": snapshot(), "x": x(), "y": y()}),
            vec![],
        ),
        (
            "type",
            "Types text into the focused field, or into a node tapped first, as key presses.",
            json!({"text": string("The text; newline is Enter."), "node": node(), "snapshot": snapshot(), "x": x(), "y": y()}),
            vec!["text"],
        ),
        (
            "scroll",
            "Scrolls with the wheel at a node or a point: positive dy is down, dx right.",
            json!({
                "node": node(), "snapshot": snapshot(), "x": x(), "y": y(),
                "dx": integer("Wheel steps right.", -protocol::MAX_SCROLL, protocol::MAX_SCROLL),
                "dy": integer("Wheel steps down.", -protocol::MAX_SCROLL, protocol::MAX_SCROLL),
            }),
            vec![],
        ),
        ("back", "Presses the back key.", json!({}), vec![]),
        (
            "key",
            "Presses one key, named as KeyEvent.KEYCODE_ names it: ENTER, DEL, TAB, DPAD_DOWN...",
            json!({"key": string("The key's name.")}),
            vec!["key"],
        ),
        (
            "wait",
            "Waits until a visible node's text, description or hint holds text, or until no window \
             drew for 500 ms with idle; returns the node found.",
            json!({
                "text": string("Text to wait for."),
                "idle": {"type": "boolean", "description": "Wait for the display to be idle instead."},
                "timeout_ms": integer("How long to wait.", 1, protocol::MAX_WAIT_MS as i64),
            }),
            vec![],
        ),
    ];
    tools
        .into_iter()
        .map(|(name, description, own, required)| {
            let mut properties = Map::new();
            if name != "list" {
                properties.insert("package".into(), string("The app's package name."));
                properties.insert(
                    "profile".into(),
                    string("The app's profile, when not the default."),
                );
            }
            if let Value::Object(own) = own {
                properties.extend(own);
            }
            let mut required: Vec<&str> = required;
            if name != "list" {
                required.insert(0, "package");
            }
            json!({
                "name": name,
                "description": description,
                "inputSchema": {"type": "object", "properties": properties, "required": required},
            })
        })
        .collect()
}

/// A tool's answer: content for the agent, an error it can act on, or a JSON-RPC error.
enum Outcome {
    Content(Vec<Value>),
    Failed(String),
}

fn text(text: impl Into<String>) -> Outcome {
    Outcome::Content(vec![json!({"type": "text", "text": text.into()})])
}

/// Whether `request` only reads the app, so sending it twice changes nothing.
fn reads_only(request: &Request) -> bool {
    matches!(
        request,
        Request::Hello
            | Request::Status
            | Request::Ui(_)
            | Request::Screenshot(_)
            | Request::Wait(_)
    )
}

/// Whether the app closed this idle connection: it is readable, which an idle one never is unless
/// its end came.
fn closed(stream: &UnixStream) -> bool {
    let mut polled = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(stream),
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one pollfd of an open socket, and no waiting.
    let ready = unsafe { libc::poll(&mut polled, 1, 0) };
    ready != 0
}

struct Server {
    dir: Option<PathBuf>,
    /// One open connection per app, so its session, and accessibility, last between calls.
    connections: HashMap<(String, String), UnixStream>,
}

impl Server {
    fn dir(&self) -> Result<&Path, String> {
        self.dir
            .as_deref()
            .ok_or_else(|| "no control directory".to_string())
    }

    /// Calls a running app over its kept connection. One the app closed while idle is opened again
    /// before the call; a call that fails on the way is sent again only when it only reads, since
    /// the app may have acted on a tap or typed text before the connection broke.
    fn call(&mut self, app: &Instance, request: &Request) -> Result<Option<Value>, String> {
        let dir = self.dir()?.to_path_buf();
        let key = (app.package.clone(), app.profile.clone());
        let timeout = app::agent_timeout(request);
        let attempts = if reads_only(request) { 2 } else { 1 };
        for attempt in 1..=attempts {
            if self.connections.get(&key).is_some_and(closed) {
                self.connections.remove(&key);
            }
            if !self.connections.contains_key(&key) {
                if !served(&dir, &app.package, &app.profile) {
                    return Ok(None);
                }
                let socket = instance_socket(&dir, &app.package, &app.profile)?;
                match client::connect(&socket) {
                    Ok(stream) => self.connections.insert(key.clone(), stream),
                    Err(CallError::NotRunning) => return Ok(None),
                    Err(error) => return Err(format!("{}: {error}", socket.display())),
                };
            }
            let stream = &self.connections[&key];
            match client::call_on(stream, request, timeout) {
                Ok(Ok(result)) => return Ok(Some(result)),
                Ok(Err(failure)) => {
                    return Err(format!(
                        "{} refused: {} ({})",
                        app.package,
                        failure.message.escape_debug(),
                        failure.code.escape_debug()
                    ));
                }
                Err(error) => {
                    self.connections.remove(&key);
                    if attempt == attempts {
                        return Err(format!("{}: {error}", app.package));
                    }
                }
            }
        }
        Ok(None)
    }

    fn tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Outcome, (i64, String)> {
        if !tools().iter().any(|tool| tool["name"] == name) {
            return Err((INVALID_PARAMS, format!("no tool {}", name.escape_debug())));
        }
        Ok(self.run(name, arguments).unwrap_or_else(Outcome::Failed))
    }

    fn run(&mut self, name: &str, arguments: &Map<String, Value>) -> Result<Outcome, String> {
        if name == "list" {
            if !arguments.is_empty() {
                return Err("list takes no arguments".into());
            }
            return done(app::list(self.dir()?));
        }
        let mut arguments = arguments.clone();
        let package = match arguments.remove("package") {
            Some(Value::String(package)) if valid_package(&package) => package,
            Some(Value::String(package)) => {
                return Err(format!("{} is not a package name", package.escape_debug()));
            }
            _ => return Err("a package name is required".into()),
        };
        let profile = match arguments.remove("profile") {
            None => DEFAULT_PROFILE.to_string(),
            Some(Value::String(profile)) if valid_profile(&profile) => profile,
            Some(_) => return Err("profile is not a profile name".into()),
        };
        let app = Instance { package, profile };
        let dir = self.dir()?.to_path_buf();
        match name {
            "status" | "stop" if !arguments.is_empty() => {
                Err(format!("{name} takes only the package"))
            }
            "status" => done(app::status(&dir, &app)),
            "stop" => {
                // The app's process exits: its connection goes with it.
                self.connections
                    .remove(&(app.package.clone(), app.profile.clone()));
                done(app::stop(&dir, &app))
            }
            "start" | "intent" => {
                let data = match arguments.remove("data_dir") {
                    Some(Value::String(dir))
                        if name == "start" && Path::new(&dir).is_absolute() =>
                    {
                        Some(PathBuf::from(dir))
                    }
                    Some(_) => return Err("data_dir takes an absolute directory, for start".into()),
                    None => None,
                };
                let intent = Intent::from_json(&Value::Object(arguments))
                    .map_err(|failure| failure.message)?;
                if name == "start" {
                    done(app::start(&dir, &app, intent, data, None))
                } else {
                    if intent == Intent::default() {
                        return Err("intent names an activity, an action or data".into());
                    }
                    done(app::send_intent(&dir, &app, intent))
                }
            }
            command => {
                // The command's own arguments, checked by the protocol as a request from anyone is.
                if let Some(key) = arguments
                    .keys()
                    .find(|key| matches!(key.as_str(), "v" | "id" | "cmd" | "intent"))
                {
                    return Err(format!("unknown argument {key}"));
                }
                arguments.insert("v".into(), protocol::VERSION.into());
                arguments.insert("cmd".into(), command.into());
                let line = Value::Object(arguments).to_string();
                let request = protocol::parse_request(line.as_bytes())
                    .map_err(|(_, failure)| failure.message)?
                    .command;
                let Some(result) = self.call(&app, &request)? else {
                    return Err(format!("{} is not running", app.package));
                };
                agent_outcome(&request, result)
            }
        }
    }
}

fn done(result: Result<Done, String>) -> Result<Outcome, String> {
    match result? {
        Done::Printed(report) => Ok(text(report.trim_end())),
        Done::NotRunning(package, _) => Err(format!("{package} is not running")),
    }
}

fn agent_outcome(request: &Request, result: Value) -> Result<Outcome, String> {
    let field = |key| app::field(&result, key).into_owned();
    Ok(match request {
        Request::Screenshot(_) => {
            let data = app::screenshot_png(&result)?;
            let png = result["png"].as_str().unwrap_or_default().to_string();
            debug_assert!(!data.is_empty());
            Outcome::Content(vec![
                json!({"type": "image", "data": png, "mimeType": "image/png"}),
                json!({"type": "text", "text": format!("{}x{} display image, scaled 1:{}", field("width"), field("height"), field("scale"))}),
            ])
        }
        Request::Tap(protocol::Place::Node { node, .. }) => text(format!(
            "tapped node {node} at {},{}",
            field("x"),
            field("y")
        )),
        Request::Tap(_) => text(format!("tapped {},{}", field("x"), field("y"))),
        Request::Type(_) => text(format!(
            "typed {} characters: {} keys, {} committed",
            field("typed"),
            field("keys"),
            field("committed")
        )),
        Request::Scroll(_) => text(format!("scrolled at {},{}", field("x"), field("y"))),
        Request::Back | Request::Key(_) => text(format!("pressed {}", field("key"))),
        _ => text(result.to_string()),
    })
}

fn error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// The answer to one message, none for a notification.
fn answer(server: &mut Server, line: &[u8]) -> Option<Value> {
    let message: Value = match serde_json::from_slice(line) {
        Ok(message) => message,
        Err(problem) => {
            return Some(error(
                &Value::Null,
                PARSE_ERROR,
                &format!("not JSON: {problem}"),
                None,
            ));
        }
    };
    let Value::Object(message) = message else {
        return Some(error(
            &Value::Null,
            INVALID_REQUEST,
            "a message is one JSON object; batches are not served",
            None,
        ));
    };
    let id = message.get("id").cloned();
    let valid_id = matches!(&id, Some(Value::String(_) | Value::Number(_)));
    let reply_id = if valid_id {
        id.clone().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if message.get("jsonrpc") != Some(&json!("2.0")) {
        return Some(error(
            &reply_id,
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
            None,
        ));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to us: this server sends no requests, so there is nothing to match.
        return None;
    };
    // Notifications, such as notifications/initialized and cancellations, need no answer.
    id.as_ref()?;
    if !valid_id {
        return Some(error(
            &Value::Null,
            INVALID_REQUEST,
            "id must be a string or a number",
            None,
        ));
    }
    let params = match message.get("params") {
        None => Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => {
            return Some(error(
                &reply_id,
                INVALID_PARAMS,
                "params must be an object",
                None,
            ));
        }
    };
    let requested = params
        .get("_meta")
        .and_then(|meta| meta.get(VERSION_META))
        .and_then(Value::as_str);
    if let Some(requested) = requested
        && !VERSIONS.contains(&requested)
    {
        let data = json!({"supported": VERSIONS, "requested": requested});
        return Some(error(
            &reply_id,
            UNSUPPORTED_VERSION,
            "Unsupported protocol version",
            Some(data),
        ));
    }
    let info = json!({"name": "uxdroid", "version": env!("CARGO_PKG_VERSION")});
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|asked| VERSIONS[1..].contains(asked))
                .unwrap_or(LATEST_INITIALIZE);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": info,
                "instructions": INSTRUCTIONS,
            })
        }
        "server/discover" => json!({
            "resultType": "complete",
            "supportedVersions": VERSIONS,
            "capabilities": {"tools": {"listChanged": false}},
            "_meta": {"io.modelcontextprotocol/serverInfo": info},
            "instructions": INSTRUCTIONS,
            "ttlMs": 3_600_000,
            "cacheScope": "public",
        }),
        "ping" => json!({}),
        "tools/list" => json!({
            "resultType": "complete",
            "tools": tools(),
            "ttlMs": 3_600_000,
            "cacheScope": "public",
        }),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(
                    &reply_id,
                    INVALID_PARAMS,
                    "tools/call needs a tool name",
                    None,
                ));
            };
            let arguments = match params.get("arguments") {
                None => Map::new(),
                Some(Value::Object(arguments)) => arguments.clone(),
                Some(_) => {
                    return Some(error(
                        &reply_id,
                        INVALID_PARAMS,
                        "arguments must be an object",
                        None,
                    ));
                }
            };
            match server.tool(name, &arguments) {
                Ok(Outcome::Content(content)) => {
                    json!({"resultType": "complete", "content": content, "isError": false})
                }
                Ok(Outcome::Failed(problem)) => json!({
                    "resultType": "complete",
                    "content": [{"type": "text", "text": problem}],
                    "isError": true,
                }),
                Err((code, message)) => return Some(error(&reply_id, code, &message, None)),
            }
        }
        other => {
            return Some(error(
                &reply_id,
                METHOD_NOT_FOUND,
                &format!("no method {}", other.escape_debug()),
                None,
            ));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": reply_id, "result": result}))
}

/// One line of at most `MAX_MESSAGE` bytes; `Some(None)` for a longer one, skipped to its end.
fn read_message(input: &mut impl BufRead) -> std::io::Result<Option<Option<Vec<u8>>>> {
    let mut line = Vec::new();
    let mut overflow = false;
    loop {
        let buffer = match input.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(match (line.is_empty(), overflow) {
                (true, false) => None,
                (_, true) => Some(None),
                (false, false) => Some(Some(line)),
            });
        }
        let (taken, done) = match buffer.iter().position(|&byte| byte == b'\n') {
            Some(end) => (end, true),
            None => (buffer.len(), false),
        };
        if !overflow {
            if line.len() + taken > MAX_MESSAGE {
                overflow = true;
                line.clear();
            } else {
                line.extend_from_slice(&buffer[..taken]);
            }
        }
        input.consume(taken + usize::from(done));
        if done {
            return Ok(Some(if overflow { None } else { Some(line) }));
        }
    }
}

pub fn run_mcp() -> ExitCode {
    let mut server = Server {
        dir: control_dir()
            .map_err(|problem| eprintln!("uxdroid mcp: {problem}"))
            .ok(),
        connections: HashMap::new(),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stdout().lock();
    loop {
        let reply = match read_message(&mut input) {
            Ok(None) => return ExitCode::SUCCESS,
            Ok(Some(None)) => Some(error(
                &Value::Null,
                INVALID_REQUEST,
                &format!("a message holds at most {MAX_MESSAGE} bytes"),
                None,
            )),
            Ok(Some(Some(line))) if line.iter().all(u8::is_ascii_whitespace) => None,
            Ok(Some(Some(line))) => answer(&mut server, &line),
            Err(problem) => {
                eprintln!("uxdroid mcp: cannot read stdin: {problem}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(reply) = reply {
            // serde_json escapes newlines inside strings, so each message is one line.
            if writeln!(output, "{reply}")
                .and_then(|()| output.flush())
                .is_err()
            {
                return ExitCode::SUCCESS;
            }
        }
    }
}
