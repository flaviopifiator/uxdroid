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

pub const BAD_REQUEST: &str = "bad_request";
pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
pub const UNKNOWN_COMMAND: &str = "unknown_command";
pub const TOO_LONG: &str = "too_long";
pub const NOT_ALLOWED: &str = "not_allowed";
pub const BUSY: &str = "busy";
pub const FAILED: &str = "failed";

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
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Hello => "hello",
            Command::Status => "status",
            Command::Start(_) => "start",
            Command::Intent(_) => "intent",
            Command::Stop => "stop",
        }
    }

    /// The commands of version 1, as `hello` lists them.
    pub const NAMES: [&'static str; 5] = ["hello", "status", "start", "intent", "stop"];
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
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "v" | "id" | "cmd" | "intent"))
    {
        return Err(refuse(bad(&format!(
            "unknown field {}",
            key.escape_debug()
        ))));
    }
    let name = object
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| refuse(bad("cmd must be a string")))?;
    let intent = |required: bool| match object.get("intent") {
        Some(intent) => Intent::from_json(intent).map_err(refuse),
        None if required => Err(refuse(bad(&format!("{name} needs an intent")))),
        None => Ok(Intent::default()),
    };
    let command = match name {
        "hello" => Command::Hello,
        "status" => Command::Status,
        "start" => Command::Start(intent(false)?),
        "intent" => Command::Intent(intent(true)?),
        "stop" => Command::Stop,
        other => {
            return Err(refuse(Failure::new(
                UNKNOWN_COMMAND,
                &format!("no command {} in version {VERSION}", other.escape_debug()),
            )));
        }
    };
    if !matches!(command, Command::Start(_) | Command::Intent(_)) && object.contains_key("intent") {
        return Err(refuse(bad(&format!("{name} takes no intent"))));
    }
    Ok(Request { id, command })
}

/// A request as one line, newline included.
pub fn request_line(id: Option<&Value>, command: &Command) -> String {
    let mut object = Map::new();
    object.insert("v".into(), VERSION.into());
    if let Some(id) = id {
        object.insert("id".into(), id.clone());
    }
    object.insert("cmd".into(), command.name().into());
    if let Command::Start(intent) | Command::Intent(intent) = command {
        object.insert("intent".into(), intent.to_json());
    }
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
