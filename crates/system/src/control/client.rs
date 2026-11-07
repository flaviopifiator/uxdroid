//! Calls a running app through its control socket. The answer is read as hostile input too: a
//! socket file can be anything that answers.

use std::io::{BufReader, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::protocol::{self, Command, Failure, LineError};
use super::server::send_all;

#[derive(Debug)]
pub enum CallError {
    /// No app answers on that socket: none was started, or it ended.
    NotRunning,
    Io(std::io::Error),
    /// What came back is not an answer.
    Protocol(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NotRunning => write!(f, "the app is not running"),
            CallError::Io(error) => write!(f, "{error}"),
            CallError::Protocol(problem) => write!(f, "the app's answer is not valid: {problem}"),
        }
    }
}

pub fn connect(path: &Path) -> Result<UnixStream, CallError> {
    UnixStream::connect(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => CallError::NotRunning,
        _ => CallError::Io(error),
    })
}

/// Sends `command` and waits at most `timeout` for each read and write of its answer.
pub fn call(
    path: &Path,
    command: &Command,
    timeout: Duration,
) -> Result<Result<Value, Failure>, CallError> {
    let stream = connect(path)?;
    call_on(&stream, command, timeout)
}

pub fn call_on(
    stream: &UnixStream,
    command: &Command,
    timeout: Duration,
) -> Result<Result<Value, Failure>, CallError> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(CallError::Io)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(CallError::Io)?;
    // An app that refuses the peer answers and closes without reading, so the write can fail with
    // EPIPE or ECONNRESET while its refusal waits to be read: read it before giving up.
    let sent = send_all(
        stream,
        protocol::request_line(Some(&json!(1)), command).as_bytes(),
    );
    if let Err(error) = &sent
        && !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        )
    {
        return Err(CallError::Io(sent.unwrap_err()));
    }
    let line = match protocol::read_line_within(&mut BufReader::new(stream), protocol::MAX_ANSWER) {
        Ok(Some(line)) => line,
        Ok(None) | Err(LineError::Io(_)) if sent.is_err() => {
            return Err(CallError::Io(sent.unwrap_err()));
        }
        Ok(None) => {
            return Err(CallError::Protocol(
                "the app closed the connection without answering".into(),
            ));
        }
        Err(LineError::TooLong) => {
            return Err(CallError::Protocol("the answer is too long".into()));
        }
        Err(LineError::Io(error)) => return Err(CallError::Io(error)),
    };
    protocol::parse_response(&line).map_err(CallError::Protocol)
}
