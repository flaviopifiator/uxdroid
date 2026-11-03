//! Version 1 of the control protocol: one JSON object per line each way, documented in
//! `docs/architecture.md`. Every request is checked here, before anything in the app sees it, and a
//! request that does not fit is refused with a code and a reason, never with a panic.

use std::io::BufRead;

use serde_json::{Map, Value, json};

/// The only version this side speaks. A request must name it.
pub const VERSION: u64 = 1;
/// The longest request accepted, newline excluded. Longer ones close the connection.
pub const MAX_LINE: usize = 64 * 1024;
/// The longest answer a client reads: room for what later versions report, such as a view tree.
pub const MAX_ANSWER: usize = 16 * 1024 * 1024;
/// The longest class name, action or extra key.
pub const MAX_NAME: usize = 255;
/// The longest data URI or extra value.
pub const MAX_VALUE: usize = 4096;
/// The most extras one intent carries.
pub const MAX_EXTRAS: usize = 64;
const MAX_ID: usize = 64;
/// `ui`: nodes and depth of one snapshot.
pub const DEFAULT_NODES: u32 = 1000;
pub const MAX_NODES: u32 = 5000;
pub const DEFAULT_DEPTH: u32 = 64;
pub const MAX_DEPTH: u32 = 128;
/// `screenshot`: the longest side of the image, which a larger display is scaled down to.
pub const DEFAULT_SHOT_SIZE: u32 = 2048;
pub const MIN_SHOT_SIZE: u32 = 16;
pub const MAX_SHOT_SIZE: u32 = 4096;
/// `screenshot`: the most windows a display holds that one can be picked from.
pub const MAX_WINDOWS: u32 = 64;
/// Display coordinates, as Wayland surfaces measure them.
pub const MAX_COORDINATE: i64 = 16383;
/// `type`: bytes of text; `wait`: bytes of the text waited for.
pub const MAX_TEXT: usize = 4096;
pub const MAX_WAIT_TEXT: usize = 1024;
/// `scroll`: wheel steps on each axis.
pub const MAX_SCROLL: i64 = 50;
/// `wait`: how long it waits, in milliseconds.
pub const DEFAULT_WAIT_MS: u32 = 10_000;
pub const MAX_WAIT_MS: u32 = 60_000;
/// `key`: a key's name, as `KeyEvent.KEYCODE_<name>` spells it.
const MAX_KEY_NAME: usize = 32;

pub const BAD_REQUEST: &str = "bad_request";
pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
pub const UNKNOWN_COMMAND: &str = "unknown_command";
pub const TOO_LONG: &str = "too_long";
pub const NOT_ALLOWED: &str = "not_allowed";
pub const BUSY: &str = "busy";
pub const FAILED: &str = "failed";
/// `tap`, `type` and `scroll` name a node the last `ui` snapshot does not hold.
pub const NO_SUCH_NODE: &str = "no_such_node";
/// They name a snapshot that is no longer the last one.
pub const STALE_SNAPSHOT: &str = "stale_snapshot";
/// The app shows no window, or not the one named.
pub const NO_WINDOW: &str = "no_window";
/// `wait` ran out of time.
pub const TIMEOUT: &str = "timeout";

/// What a client asks of a running app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// The protocol versions and commands this app serves, its package and process.
    Hello,
    /// The process and its activities.
    Status,
    /// A launch: the intent goes to the running app as a second launch would.
    Start(Intent),
    /// An intent for the running app, which must name what to start or match a filter.
    Intent(Intent),
    /// Finishes every activity, then the process exits.
    Stop,
    /// The windows' view hierarchy and accessibility nodes, as a snapshot.
    Ui(Ui),
    /// The composed display, or one window, as PNG.
    Screenshot(Screenshot),
    /// A finger down and up, through the seat.
    Tap(Place),
    /// Key presses that type text into the focused field, or into a node tapped first.
    Type(Typing),
    /// Wheel steps at a point.
    Scroll(Scroll),
    /// The back key.
    Back,
    /// One key, by its `KeyEvent.KEYCODE_` name.
    Key(String),
    /// Waits until a node shows text, or until no window draws.
    Wait(Wait),
    /// Internal, never read from a request: text the seat's keymap cannot type, committed through
    /// the focused view's `InputConnection`.
    Commit(String),
    /// Internal, never read from a request: the last agent left, so accessibility turns off.
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ui {
    pub max_nodes: u32,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    /// One window, by its place in the display's stack, bottom first; none for the whole display.
    pub window: Option<u32>,
    pub max_size: u32,
}

/// Where an action happens: a node of a `ui` snapshot, at its center, or a point on the display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Node { node: u32, snapshot: Option<u64> },
    Point { x: i32, y: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typing {
    /// Data only: each character becomes key presses, nothing is ever evaluated.
    pub text: String,
    /// Tapped first, to focus it.
    pub node: Option<Place>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scroll {
    pub at: Place,
    /// Wheel steps: positive is right and down.
    pub dx: i32,
    pub dy: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// A visible node whose text, description or hint holds this.
    Text(String),
    /// No window posted a frame for a while.
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    pub condition: Condition,
    pub timeout_ms: u32,
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Hello => "hello",
            Command::Status => "status",
            Command::Start(_) => "start",
            Command::Intent(_) => "intent",
            Command::Stop => "stop",
            Command::Ui(_) => "ui",
            Command::Screenshot(_) => "screenshot",
            Command::Tap(_) => "tap",
            Command::Type(_) => "type",
            Command::Scroll(_) => "scroll",
            Command::Back => "back",
            Command::Key(_) => "key",
            Command::Wait(_) => "wait",
            Command::Commit(_) => "commit",
            Command::Release => "release",
        }
    }

    /// The commands of version 1, as `hello` lists them.
    pub const NAMES: [&'static str; 13] = [
        "hello",
        "status",
        "start",
        "intent",
        "stop",
        "ui",
        "screenshot",
        "tap",
        "type",
        "scroll",
        "back",
        "key",
        "wait",
    ];

    /// The fields a request of this command holds besides `v`, `id` and `cmd`.
    pub fn fields(&self) -> Map<String, Value> {
        let mut fields = Map::new();
        let mut put = |key: &str, value: Value| {
            fields.insert(key.into(), value);
        };
        match self {
            Command::Start(intent) | Command::Intent(intent) => put("intent", intent.to_json()),
            Command::Ui(ui) => {
                put("max_nodes", ui.max_nodes.into());
                put("max_depth", ui.max_depth.into());
            }
            Command::Screenshot(shot) => {
                if let Some(window) = shot.window {
                    put("window", window.into());
                }
                put("max_size", shot.max_size.into());
            }
            Command::Tap(place) => place.put(&mut put),
            Command::Type(typing) => {
                put("text", typing.text.clone().into());
                if let Some(place) = &typing.node {
                    place.put(&mut put);
                }
            }
            Command::Scroll(scroll) => {
                scroll.at.put(&mut put);
                put("dx", scroll.dx.into());
                put("dy", scroll.dy.into());
            }
            Command::Key(key) => put("key", key.clone().into()),
            Command::Wait(wait) => {
                match &wait.condition {
                    Condition::Text(text) => put("text", text.clone().into()),
                    Condition::Idle => put("idle", true.into()),
                }
                put("timeout_ms", wait.timeout_ms.into());
            }
            Command::Commit(text) => put("text", text.clone().into()),
            Command::Hello | Command::Status | Command::Stop | Command::Back | Command::Release => {
            }
        }
        fields
    }
}

impl Place {
    fn put(&self, put: &mut impl FnMut(&str, Value)) {
        match self {
            Place::Node { node, snapshot } => {
                put("node", (*node).into());
                if let Some(snapshot) = snapshot {
                    put("snapshot", (*snapshot).into());
                }
            }
            Place::Point { x, y } => {
                put("x", (*x).into());
                put("y", (*y).into());
            }
        }
    }
}

/// The arguments of one request, each taken once, so what is left over can be refused.
struct Arguments<'a> {
    name: &'a str,
    object: &'a Map<String, Value>,
    taken: Vec<&'static str>,
}

impl Arguments<'_> {
    fn get(&mut self, key: &'static str) -> Option<&Value> {
        self.taken.push(key);
        self.object.get(key)
    }

    fn integer(
        &mut self,
        key: &'static str,
        range: std::ops::RangeInclusive<i64>,
    ) -> Result<Option<i64>, Failure> {
        let name = self.name;
        let Some(value) = self.get(key) else {
            return Ok(None);
        };
        value
            .as_i64()
            .filter(|number| range.contains(number))
            .map(Some)
            .ok_or_else(|| {
                bad(&format!(
                    "{name}'s {key} must be an integer from {} to {}",
                    range.start(),
                    range.end()
                ))
            })
    }

    fn string(
        &mut self,
        key: &'static str,
        max: usize,
        lines: bool,
    ) -> Result<Option<String>, Failure> {
        let name = self.name;
        let Some(value) = self.get(key) else {
            return Ok(None);
        };
        let text = value
            .as_str()
            .ok_or_else(|| bad(&format!("{name}'s {key} must be a string")))?;
        if text.is_empty() || text.len() > max {
            return Err(bad(&format!("{name}'s {key} holds 1 to {max} bytes")));
        }
        let allowed = |c: char| !c.is_control() || (lines && (c == '\n' || c == '\t'));
        if !text.chars().all(allowed) {
            return Err(bad(&format!(
                "{name}'s {key} must not hold control characters"
            )));
        }
        Ok(Some(text.to_string()))
    }

    /// A node, or a point, or neither when `optional`.
    fn place(&mut self, optional: bool) -> Result<Option<Place>, Failure> {
        let name = self.name;
        let node = self.integer("node", 1..=u32::MAX as i64)?;
        let snapshot = self.integer("snapshot", 0..=i64::MAX)?;
        let x = self.integer("x", 0..=MAX_COORDINATE)?;
        let y = self.integer("y", 0..=MAX_COORDINATE)?;
        match (node, snapshot, x, y) {
            (Some(node), snapshot, None, None) => Ok(Some(Place::Node {
                node: node as u32,
                snapshot: snapshot.map(|snapshot| snapshot as u64),
            })),
            (None, None, Some(x), Some(y)) => Ok(Some(Place::Point {
                x: x as i32,
                y: y as i32,
            })),
            (None, None, None, None) if optional => Ok(None),
            _ => Err(bad(&format!(
                "{name} takes a node (and its snapshot), or x and y"
            ))),
        }
    }
}

/// An intent for one of the app's own activities: only names and strings, so nothing in it can make
/// the app load code it does not already have.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Intent {
    /// A class name, or `.Name` relative to the package.
    pub activity: Option<String>,
    pub action: Option<String>,
    pub data: Option<String>,
    /// String extras, in the order given.
    pub extras: Vec<(String, String)>,
    /// `Intent.setFlags`.
    pub flags: u32,
}

impl Intent {
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        for (key, value) in [
            ("activity", &self.activity),
            ("action", &self.action),
            ("data", &self.data),
        ] {
            if let Some(value) = value {
                object.insert(key.into(), Value::String(value.clone()));
            }
        }
        if !self.extras.is_empty() {
            let extras = self
                .extras
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())));
            object.insert("extras".into(), Value::Object(extras.collect()));
        }
        if self.flags != 0 {
            object.insert("flags".into(), self.flags.into());
        }
        Value::Object(object)
    }

    pub fn from_json(value: &Value) -> Result<Intent, Failure> {
        let object = value
            .as_object()
            .ok_or_else(|| bad("intent must be an object"))?;
        let mut intent = Intent::default();
        for (key, value) in object {
            match key.as_str() {
                "activity" => {
                    let name = text(value, key, MAX_NAME)?;
                    let valid = !name.is_empty()
                        && name.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'$'
                        });
                    if !valid {
                        return Err(bad("activity must be a class name"));
                    }
                    intent.activity = Some(name);
                }
                "action" => intent.action = Some(nonempty(text(value, key, MAX_NAME)?, key)?),
                "data" => intent.data = Some(nonempty(text(value, key, MAX_VALUE)?, key)?),
                "extras" => {
                    let extras = value
                        .as_object()
                        .ok_or_else(|| bad("extras must be an object of strings"))?;
                    if extras.len() > MAX_EXTRAS {
                        return Err(bad(&format!("at most {MAX_EXTRAS} extras")));
                    }
                    for (name, value) in extras {
                        if name.is_empty()
                            || name.len() > MAX_NAME
                            || name.chars().any(char::is_control)
                        {
                            return Err(bad(
                                "an extra's key must be 1 to 255 characters, none of them control",
                            ));
                        }
                        let value = value
                            .as_str()
                            .ok_or_else(|| bad("extras must be strings"))?;
                        if value.len() > MAX_VALUE {
                            return Err(bad(&format!("an extra holds at most {MAX_VALUE} bytes")));
                        }
                        intent.extras.push((name.clone(), value.to_string()));
                    }
                }
                "flags" => {
                    intent.flags = value
                        .as_u64()
                        .and_then(|flags| u32::try_from(flags).ok())
                        .ok_or_else(|| bad("flags must be an integer from 0 to 4294967295"))?;
                }
                other => {
                    return Err(bad(&format!(
                        "unknown intent field {}",
                        other.escape_debug()
                    )));
                }
            }
        }
        Ok(intent)
    }
}

fn text(value: &Value, key: &str, max: usize) -> Result<String, Failure> {
    let text = value
        .as_str()
        .ok_or_else(|| bad(&format!("{key} must be a string")))?;
    if text.len() > max {
        return Err(bad(&format!("{key} holds at most {max} bytes")));
    }
    if text.chars().any(char::is_control) {
        return Err(bad(&format!("{key} must not hold control characters")));
    }
    Ok(text.to_string())
}

fn nonempty(text: String, key: &str) -> Result<String, Failure> {
    if text.is_empty() {
        return Err(bad(&format!("{key} must not be empty")));
    }
    Ok(text)
}

/// A refusal: a stable code for programs and a message for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

impl Failure {
    pub fn new(code: &str, message: &str) -> Failure {
        Failure {
            code: code.into(),
            message: message.into(),
        }
    }
}

fn bad(message: &str) -> Failure {
    Failure::new(BAD_REQUEST, message)
}

#[derive(Debug, PartialEq)]
pub struct Request {
    /// Echoed in the answer: a number or a short string.
    pub id: Option<Value>,
    pub command: Command,
}

/// Reads one request. A refusal keeps the request's id when it had a valid one, so its answer can
/// be matched.
pub fn parse_request(line: &[u8]) -> Result<Request, (Option<Value>, Failure)> {
    let value: Value =
        serde_json::from_slice(line).map_err(|error| (None, bad(&format!("not JSON: {error}"))))?;
    let Value::Object(object) = value else {
        return Err((None, bad("a request must be a JSON object")));
    };
    let id = match object.get("id") {
        None => None,
        Some(id @ Value::Number(number)) if number.is_u64() => Some(id.clone()),
        Some(id @ Value::String(text)) if text.len() <= MAX_ID => Some(id.clone()),
        Some(_) => {
            return Err((
                None,
                bad("id must be a non-negative integer or a string of at most 64 bytes"),
            ));
        }
    };
    let refuse = |failure: Failure| (id.clone(), failure);
    if object.get("v").and_then(Value::as_u64) != Some(VERSION) {
        return Err(refuse(Failure::new(
            UNSUPPORTED_VERSION,
            &format!("this app speaks version {VERSION}: requests must hold \"v\":{VERSION}"),
        )));
    }
    if let Some(key) = object.keys().find(|key| {
        !matches!(key.as_str(), "v" | "id" | "cmd") && !ARGUMENTS.contains(&key.as_str())
    }) {
        return Err(refuse(bad(&format!(
            "unknown field {}",
            key.escape_debug()
        ))));
    }
    let name = object
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| refuse(bad("cmd must be a string")))?;
    if !Command::NAMES.contains(&name) {
        return Err(refuse(Failure::new(
            UNKNOWN_COMMAND,
            &format!("no command {} in version {VERSION}", name.escape_debug()),
        )));
    }
    let mut arguments = Arguments {
        name,
        object: &object,
        taken: vec!["v", "id", "cmd"],
    };
    let command = command(name, &mut arguments).map_err(refuse)?;
    if let Some(key) = object
        .keys()
        .find(|key| !arguments.taken.contains(&key.as_str()))
    {
        return Err(refuse(bad(&format!("{name} takes no {key}"))));
    }
    Ok(Request { id, command })
}

/// Every field some command takes.
const ARGUMENTS: [&str; 15] = [
    "intent",
    "max_nodes",
    "max_depth",
    "window",
    "max_size",
    "node",
    "snapshot",
    "x",
    "y",
    "text",
    "dx",
    "dy",
    "key",
    "idle",
    "timeout_ms",
];

fn command(name: &str, arguments: &mut Arguments) -> Result<Command, Failure> {
    let mut intent = |required: bool| match arguments.get("intent") {
        Some(intent) => Intent::from_json(intent),
        None if required => Err(bad(&format!("{name} needs an intent"))),
        None => Ok(Intent::default()),
    };
    Ok(match name {
        "hello" => Command::Hello,
        "status" => Command::Status,
        "start" => Command::Start(intent(false)?),
        "intent" => Command::Intent(intent(true)?),
        "stop" => Command::Stop,
        "ui" => Command::Ui(Ui {
            max_nodes: arguments
                .integer("max_nodes", 1..=MAX_NODES as i64)?
                .map_or(DEFAULT_NODES, |n| n as u32),
            max_depth: arguments
                .integer("max_depth", 1..=MAX_DEPTH as i64)?
                .map_or(DEFAULT_DEPTH, |n| n as u32),
        }),
        "screenshot" => Command::Screenshot(Screenshot {
            window: arguments
                .integer("window", 0..=MAX_WINDOWS as i64 - 1)?
                .map(|n| n as u32),
            max_size: arguments
                .integer("max_size", MIN_SHOT_SIZE as i64..=MAX_SHOT_SIZE as i64)?
                .map_or(DEFAULT_SHOT_SIZE, |n| n as u32),
        }),
        "tap" => Command::Tap(
            arguments
                .place(false)?
                .ok_or_else(|| bad("tap needs a place"))?,
        ),
        "type" => {
            let text = arguments
                .string("text", MAX_TEXT, true)?
                .ok_or_else(|| bad("type needs a text"))?;
            Command::Type(Typing {
                text,
                node: arguments.place(true)?,
            })
        }
        "scroll" => {
            let at = arguments
                .place(false)?
                .ok_or_else(|| bad("scroll needs a place"))?;
            let dx = arguments
                .integer("dx", -MAX_SCROLL..=MAX_SCROLL)?
                .unwrap_or(0) as i32;
            let dy = arguments
                .integer("dy", -MAX_SCROLL..=MAX_SCROLL)?
                .unwrap_or(0) as i32;
            if (dx, dy) == (0, 0) {
                return Err(bad("scroll needs dx or dy, in wheel steps"));
            }
            Command::Scroll(Scroll { at, dx, dy })
        }
        "back" => Command::Back,
        "key" => {
            let key = arguments
                .string("key", MAX_KEY_NAME, false)?
                .ok_or_else(|| bad("key needs a key name"))?;
            if !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(bad(
                    "a key is named as KeyEvent.KEYCODE_ names it, such as ENTER",
                ));
            }
            Command::Key(key)
        }
        "wait" => {
            let text = arguments.string("text", MAX_WAIT_TEXT, false)?;
            let idle = match arguments.get("idle") {
                None => false,
                Some(Value::Bool(true)) => true,
                Some(_) => return Err(bad("wait's idle must be true")),
            };
            let condition = match (text, idle) {
                (Some(text), false) => Condition::Text(text),
                (None, true) => Condition::Idle,
                _ => return Err(bad("wait takes a text or idle, one of them")),
            };
            let timeout_ms = arguments
                .integer("timeout_ms", 1..=MAX_WAIT_MS as i64)?
                .map_or(DEFAULT_WAIT_MS, |n| n as u32);
            Command::Wait(Wait {
                condition,
                timeout_ms,
            })
        }
        other => {
            return Err(Failure::new(
                UNKNOWN_COMMAND,
                &format!("no command {} in version {VERSION}", other.escape_debug()),
            ));
        }
    })
}

/// A request as one line, newline included.
pub fn request_line(id: Option<&Value>, command: &Command) -> String {
    let mut object = Map::new();
    object.insert("v".into(), VERSION.into());
    if let Some(id) = id {
        object.insert("id".into(), id.clone());
    }
    object.insert("cmd".into(), command.name().into());
    object.extend(command.fields());
    format!("{}\n", Value::Object(object))
}

fn envelope(id: Option<&Value>, ok: bool, key: &str, body: Value) -> String {
    let mut object = Map::new();
    object.insert("v".into(), VERSION.into());
    if let Some(id) = id {
        object.insert("id".into(), id.clone());
    }
    object.insert("ok".into(), ok.into());
    object.insert(key.into(), body);
    format!("{}\n", Value::Object(object))
}

pub fn response_ok(id: Option<&Value>, result: Value) -> String {
    envelope(id, true, "result", result)
}

pub fn response_error(id: Option<&Value>, failure: &Failure) -> String {
    envelope(
        id,
        false,
        "error",
        json!({"code": failure.code, "message": failure.message}),
    )
}

/// Reads an answer: the result, or the app's refusal. `Err` means the line is not an answer.
pub fn parse_response(line: &[u8]) -> Result<Result<Value, Failure>, String> {
    let value: Value =
        serde_json::from_slice(line).map_err(|error| format!("not JSON: {error}"))?;
    let object = value.as_object().ok_or("an answer must be a JSON object")?;
    if object.get("v").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!("the answer is not version {VERSION}"));
    }
    match object.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(Ok(object.get("result").cloned().unwrap_or(Value::Null))),
        Some(false) => {
            let error = object
                .get("error")
                .and_then(Value::as_object)
                .ok_or("a refusal has no error")?;
            let field = |key: &str| {
                error
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or(format!("a refusal has no {key}"))
            };
            Ok(Err(Failure {
                code: field("code")?,
                message: field("message")?,
            }))
        }
        None => Err("an answer must say whether it is ok".into()),
    }
}

#[derive(Debug)]
pub enum LineError {
    TooLong,
    Io(std::io::Error),
}

/// One line without its newline, or `None` at the end. Never holds more than `MAX_LINE` bytes, so a
/// peer that never sends a newline cannot make this side grow.
pub fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, LineError> {
    read_line_within(reader, MAX_LINE)
}

/// `read_line` with another limit.
pub fn read_line_within(
    reader: &mut impl BufRead,
    max: usize,
) -> Result<Option<Vec<u8>>, LineError> {
    let mut line = Vec::new();
    loop {
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(LineError::Io(error)),
        };
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let (taken, done) = match buffer.iter().position(|&byte| byte == b'\n') {
            Some(end) => (end, true),
            None => (buffer.len(), false),
        };
        if line.len() + taken > max {
            return Err(LineError::TooLong);
        }
        line.extend_from_slice(&buffer[..taken]);
        reader.consume(taken + usize::from(done));
        if done {
            return Ok(Some(line));
        }
    }
}
