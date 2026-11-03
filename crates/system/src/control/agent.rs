//! Agent control: reading the screen and acting on it through the control socket.
//!
//! `ui` is answered by the app's main thread, which reads its windows' view hierarchy and
//! accessibility nodes (`dev.uxdroid.system.Agent`); this side remembers the last snapshot, so a
//! later command can name a node by its id. Every action goes through the seat, as the compositor's
//! input does: a tap is a finger down and up, typing is key presses of the compositor's keymap, a
//! scroll is wheel steps, so the app cannot tell an agent from a user. Only text the keymap cannot
//! type is committed through the focused view's `InputConnection`, as an input method would.
//! `screenshot` reads the windows' last frames, the ones the compositor shows, and composes them as
//! the compositor does. Nothing a request holds is ever run: text is typed, never evaluated.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::{self, Command, Condition, Failure, Place, Screenshot, Ui};
use super::server::{Handler, Session};
use crate::input::charmap::Charmap;
use crate::input::keys::{self, KEYCODE_BACK};

pub use super::png;

/// Seat input in display coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    TouchDown {
        x: i32,
        y: i32,
    },
    TouchUp,
    /// Wheel steps at a point: positive is right and down.
    Wheel {
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
    },
    /// One evdev key, with shift held around it when `shift`.
    Key {
        evdev: u32,
        pressed: bool,
        shift: bool,
    },
}

/// One window's last frame, where it is on the display, 0xAARRGGBB row after row.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowFrame {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

/// What the agent needs of the display: its size, its seat, and its windows' frames.
pub trait Seat: Send + Sync {
    /// The display's size; none before the first window.
    fn display(&self) -> Option<(i32, i32)>;
    /// Sends input as the compositor's seat would.
    fn send(&self, input: Input) -> Result<(), String>;
    /// How many frames the windows presented so far.
    fn presented(&self) -> u64;
    /// The windows' frames, bottom first.
    fn frames(&self) -> Vec<WindowFrame>;
    /// The seat keyboard's characters.
    fn charmap(&self) -> Arc<Charmap>;
}

/// How long a finger stays down for a tap, as a quick tap of a user's.
const TAP: Duration = Duration::from_millis(40);
/// Time for a tapped field to take focus before keys come.
const FOCUS: Duration = Duration::from_millis(100);
/// How often `wait` looks again.
const POLL: Duration = Duration::from_millis(100);
/// A display that presented nothing for this long is idle.
const QUIET: Duration = Duration::from_millis(500);
/// How long a finger or key up that failed waits before it is sent again.
const LIFT_RETRY: Duration = Duration::from_millis(20);
/// The largest `ui` answer passed on, below the protocol's 16 MiB.
const MAX_UI_ANSWER: usize = 8 << 20;
/// The largest PNG passed on, as base64.
const MAX_SHOT_ANSWER: usize = 12 << 20;

/// The last `ui` snapshot: its number and each node's bounds.
struct Snapshot {
    id: u64,
    bounds: HashMap<u32, [i32; 4]>,
}

/// The agent's side of the control socket, in front of the app's own handler.
pub struct Agent {
    app: Arc<dyn Handler>,
    seat: Arc<dyn Seat>,
    snapshot: Mutex<Option<Snapshot>>,
    /// Connections that asked for the accessibility tree and are still open.
    agents: Mutex<usize>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn refusal(code: &str, message: &str) -> Failure {
    Failure::new(code, message)
}

/// The nodes of a snapshot, depth first, each with its depth. Anything malformed is skipped.
fn nodes(answer: &Value) -> Vec<&Value> {
    let mut found = Vec::new();
    let mut stack: Vec<&Value> = answer
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|window| window.get("root"))
        .rev()
        .collect();
    while let Some(node) = stack.pop() {
        if !node.is_object() {
            continue;
        }
        found.push(node);
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            stack.extend(children.iter().rev());
        }
    }
    found
}

fn bounds(node: &Value) -> Option<[i32; 4]> {
    let list = node.get("bounds")?.as_array()?;
    let mut bounds = [0; 4];
    if list.len() != 4 {
        return None;
    }
    for (slot, value) in bounds.iter_mut().zip(list) {
        *slot = i32::try_from(value.as_i64()?).ok()?;
    }
    Some(bounds)
}

/// A node's own fields, without its children.
fn alone(node: &Value) -> Value {
    let mut node = node.clone();
    if let Some(object) = node.as_object_mut() {
        object.remove("children");
    }
    node
}

/// One step of typing: a key of the seat's keymap, or text it cannot type.
#[derive(Debug, PartialEq)]
enum Step {
    Key { evdev: u32, shift: bool },
    Commit(String),
}

/// How `text` is typed with `charmap`: each character a key press when a key types it, with shift
/// when it needs shift, else committed as text. A character on an AltGr level is committed too:
/// the seat's key path holds only shift.
fn plan(charmap: &Charmap, text: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for character in text.chars() {
        let key = u16::try_from(u32::from(character))
            .ok()
            .and_then(|unit| charmap.events(&[unit]))
            .and_then(|events| {
                let shifted = keys::META_SHIFT_ON | keys::META_SHIFT_LEFT_ON;
                let (code, shift) = match events.as_slice() {
                    [(_, code, 0), _] => (*code, false),
                    [_, (_, code, meta), _, _] if *meta == shifted => (*code, true),
                    _ => return None,
                };
                let evdev = keys::TABLE.iter().find(|m| m.key_code == code)?.evdev;
                Some(Step::Key { evdev, shift })
            });
        match (key, steps.last_mut()) {
            (Some(key), _) => steps.push(key),
            (None, Some(Step::Commit(pending))) => pending.push(character),
            (None, _) => steps.push(Step::Commit(character.to_string())),
        }
    }
    steps
}

impl Agent {
    pub fn new(app: Arc<dyn Handler>, seat: Arc<dyn Seat>) -> Agent {
        Agent {
            app,
            seat,
            snapshot: Mutex::new(None),
            agents: Mutex::new(0),
        }
    }

    fn ui(&self, session: &mut Session, ui: Ui) -> Result<Value, Failure> {
        if !session.agent {
            session.agent = true;
            *lock(&self.agents) += 1;
        }
        let answer = self.app.handle(Command::Ui(ui))?;
        if answer.to_string().len() > MAX_UI_ANSWER {
            return Err(refusal(
                protocol::FAILED,
                "the view tree is too large to answer; ask for fewer nodes with max_nodes",
            ));
        }
        let id = answer.get("snapshot").and_then(Value::as_u64).unwrap_or(0);
        let bounds = nodes(&answer)
            .into_iter()
            .filter_map(|node| {
                let id = u32::try_from(node.get("id")?.as_u64()?).ok()?;
                Some((id, bounds(node)?))
            })
            .collect();
        *lock(&self.snapshot) = Some(Snapshot { id, bounds });
        Ok(answer)
    }

    /// The display point a place names: a node's center, or a point inside the display.
    fn point(&self, place: &Place) -> Result<(i32, i32), Failure> {
        let (width, height) = self
            .seat
            .display()
            .ok_or_else(|| refusal(protocol::NO_WINDOW, "the app shows no window"))?;
        match *place {
            Place::Point { x, y } if x < width && y < height => Ok((x, y)),
            Place::Point { x, y } => Err(refusal(
                protocol::BAD_REQUEST,
                &format!("{x},{y} is outside the {width}x{height} display"),
            )),
            Place::Node { node, snapshot } => {
                let last = lock(&self.snapshot);
                let Some(last) = last.as_ref() else {
                    return Err(refusal(
                        protocol::NO_SUCH_NODE,
                        "no ui snapshot was taken yet",
                    ));
                };
                if let Some(snapshot) = snapshot
                    && snapshot != last.id
                {
                    return Err(refusal(
                        protocol::STALE_SNAPSHOT,
                        &format!("snapshot {snapshot} is not the last one, {}", last.id),
                    ));
                }
                match last.bounds.get(&node) {
                    Some(&[left, top, right, bottom]) if right > left && bottom > top => {
                        let center = ((left + right) / 2, (top + bottom) / 2);
                        let inside =
                            center.0 >= 0 && center.1 >= 0 && center.0 < width && center.1 < height;
                        if inside {
                            Ok(center)
                        } else {
                            Err(refusal(
                                protocol::NO_SUCH_NODE,
                                &format!("node {node} is outside the display"),
                            ))
                        }
                    }
                    Some(_) => Err(refusal(
                        protocol::NO_SUCH_NODE,
                        &format!("node {node} has no area on the display"),
                    )),
                    None => Err(refusal(
                        protocol::NO_SUCH_NODE,
                        &format!("snapshot {} has no node {node}", last.id),
                    )),
                }
            }
        }
    }

    fn send(&self, input: Input) -> Result<(), Failure> {
        self.seat
            .send(input)
            .map_err(|problem| refusal(protocol::FAILED, &problem))
    }

    /// Sends the up that ends a press, once more if it failed: a finger or key left down would hold
    /// the app in a gesture the agent never finishes.
    fn lift(&self, input: Input) -> Result<(), Failure> {
        self.send(input.clone()).or_else(|_| {
            std::thread::sleep(LIFT_RETRY);
            self.send(input)
        })
    }

    fn tap(&self, place: &Place) -> Result<(i32, i32), Failure> {
        let (x, y) = self.point(place)?;
        self.send(Input::TouchDown { x, y })?;
        std::thread::sleep(TAP);
        self.lift(Input::TouchUp)?;
        Ok((x, y))
    }

    fn press(&self, evdev: u32, shift: bool) -> Result<(), Failure> {
        self.send(Input::Key {
            evdev,
            pressed: true,
            shift,
        })?;
        self.lift(Input::Key {
            evdev,
            pressed: false,
            shift,
        })
    }

    fn type_text(&self, text: &str, node: Option<&Place>) -> Result<Value, Failure> {
        if let Some(node) = node {
            self.tap(node)?;
            std::thread::sleep(FOCUS);
        }
        let total = text.chars().count();
        let (mut pressed, mut committed) = (0, 0);
        // A failure says how much was typed, so the caller can type the rest instead of all again.
        let stopped = |failure: Failure, done: usize| {
            Failure::new(
                &failure.code,
                &format!(
                    "typing stopped after {done} of {total} characters: {}",
                    failure.message
                ),
            )
        };
        for step in plan(&self.seat.charmap(), text) {
            match step {
                Step::Key { evdev, shift } => {
                    self.press(evdev, shift)
                        .map_err(|failure| stopped(failure, pressed + committed))?;
                    pressed += 1;
                }
                Step::Commit(text) => {
                    self.app
                        .handle(Command::Commit(text.clone()))
                        .map_err(|failure| stopped(failure, pressed + committed))?;
                    committed += text.chars().count();
                }
            }
        }
        Ok(json!({"typed": total, "keys": pressed, "committed": committed}))
    }

    fn key(&self, name: &str) -> Result<Value, Failure> {
        let mapping = keys::TABLE
            .iter()
            .find(|mapping| mapping.label == name)
            .ok_or_else(|| {
                refusal(
                    protocol::BAD_REQUEST,
                    &format!("no key {name} on the seat's keyboard; keys are named as KeyEvent.KEYCODE_ names them"),
                )
            })?;
        self.press(mapping.evdev, false)?;
        Ok(json!({"key": name}))
    }

    fn wait(
        &self,
        session: &mut Session,
        condition: &Condition,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut presented = self.seat.presented();
        let mut quiet_since = started;
        // A snapshot the app could not take this time, such as while its main thread is busy, is
        // looked at again until the time runs out.
        let mut failed: Option<Failure> = None;
        loop {
            match condition {
                Condition::Text(text) => {
                    let ui = Ui {
                        max_nodes: protocol::MAX_NODES,
                        max_depth: protocol::MAX_DEPTH,
                    };
                    let answer = match self.ui(session, ui) {
                        Ok(answer) => {
                            failed = None;
                            answer
                        }
                        Err(failure) => {
                            failed = Some(failure);
                            Value::Null
                        }
                    };
                    let shows = |node: &&Value| {
                        node.get("visible") == Some(&Value::Bool(true))
                            && ["text", "description", "hint"].iter().any(|key| {
                                node.get(key)
                                    .and_then(Value::as_str)
                                    .is_some_and(|value| value.contains(text.as_str()))
                            })
                    };
                    if let Some(node) = nodes(&answer).into_iter().find(shows) {
                        return Ok(json!({
                            "snapshot": answer.get("snapshot").cloned().unwrap_or(Value::Null),
                            "node": alone(node),
                            "waited_ms": started.elapsed().as_millis() as u64,
                        }));
                    }
                }
                Condition::Idle => {
                    let now = self.seat.presented();
                    if now != presented {
                        presented = now;
                        quiet_since = Instant::now();
                    } else if quiet_since.elapsed() >= QUIET {
                        return Ok(json!({
                            "idle": true,
                            "waited_ms": started.elapsed().as_millis() as u64,
                        }));
                    }
                }
            }
            if Instant::now() >= deadline {
                let mut message = format!(
                    "the condition did not hold within {} ms",
                    timeout.as_millis()
                );
                if let Some(failure) = failed {
                    message.push_str(&format!("; the last ui failed: {}", failure.message));
                }
                return Err(refusal(protocol::TIMEOUT, &message));
            }
            let poll = if matches!(condition, Condition::Idle) {
                POLL / 2
            } else {
                POLL
            };
            std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn screenshot(&self, shot: &Screenshot) -> Result<Value, Failure> {
        let frames = self.seat.frames();
        let no_window = || refusal(protocol::NO_WINDOW, "the app shows no such window");
        let (width, height, pixels) = match shot.window {
            Some(index) => {
                let frame = frames.get(index as usize).ok_or_else(no_window)?;
                (frame.width, frame.height, frame.pixels.clone())
            }
            None => {
                let (width, height) = self
                    .seat
                    .display()
                    .or_else(|| frames.first().map(|frame| (frame.width, frame.height)))
                    .ok_or_else(no_window)?;
                if frames.is_empty() {
                    return Err(no_window());
                }
                (width, height, compose(&frames, width, height))
            }
        };
        if width <= 0 || height <= 0 {
            return Err(no_window());
        }
        let longest = width.max(height) as u32;
        let scale = longest.div_ceil(shot.max_size.max(1)).max(1);
        let image = scaled(&pixels, width as u32, height as u32, scale);
        let encoded = png::encode(&image)
            .ok_or_else(|| refusal(protocol::FAILED, "the frame does not fill its size"))?;
        let data = png::base64_encode(&encoded);
        if data.len() > MAX_SHOT_ANSWER {
            return Err(refusal(
                protocol::FAILED,
                "the image is too large to answer; ask for a smaller max_size",
            ));
        }
        Ok(json!({
            "width": image.width,
            "height": image.height,
            "scale": scale,
            "windows": frames.len(),
            "png": data,
        }))
    }
}

/// The frames over each other, bottom first, on a black display, as the compositor shows them.
fn compose(frames: &[WindowFrame], width: i32, height: i32) -> Vec<u32> {
    let mut display = vec![0xFF00_0000; (width.max(0) * height.max(0)) as usize];
    for frame in frames {
        if frame.pixels.len() < (frame.width.max(0) * frame.height.max(0)) as usize {
            continue;
        }
        for row in 0..frame.height {
            let y = frame.y + row;
            if !(0..height).contains(&y) {
                continue;
            }
            let left = frame.x.max(0);
            let right = (frame.x + frame.width).min(width);
            if left >= right {
                continue;
            }
            let source = (row * frame.width + (left - frame.x)) as usize;
            let target = (y * width + left) as usize;
            let length = (right - left) as usize;
            display[target..target + length]
                .copy_from_slice(&frame.pixels[source..source + length]);
        }
    }
    display
}

/// Every `scale`th pixel of each `scale`th row, as RGB.
fn scaled(pixels: &[u32], width: u32, height: u32, scale: u32) -> png::Image {
    let (out_width, out_height) = ((width / scale).max(1), (height / scale).max(1));
    let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);
    for y in 0..out_height {
        for x in 0..out_width {
            let pixel = pixels
                .get(((y * scale).min(height - 1) * width + (x * scale).min(width - 1)) as usize)
                .copied()
                .unwrap_or(0);
            rgb.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
        }
    }
    png::Image {
        width: out_width,
        height: out_height,
        rgb,
    }
}

impl Handler for Agent {
    fn handle(&self, command: Command) -> Result<Value, Failure> {
        self.handle_in(&mut Session::default(), command)
    }

    fn handle_in(&self, session: &mut Session, command: Command) -> Result<Value, Failure> {
        match command {
            Command::Ui(ui) => self.ui(session, ui),
            Command::Screenshot(shot) => self.screenshot(&shot),
            Command::Tap(place) => {
                let (x, y) = self.tap(&place)?;
                let mut result = json!({"x": x, "y": y});
                if let Place::Node { node, .. } = place {
                    result["node"] = node.into();
                }
                Ok(result)
            }
            Command::Type(typing) => self.type_text(&typing.text, typing.node.as_ref()),
            Command::Scroll(scroll) => {
                let (x, y) = self.point(&scroll.at)?;
                self.send(Input::Wheel {
                    x,
                    y,
                    dx: scroll.dx,
                    dy: scroll.dy,
                })?;
                Ok(json!({"x": x, "y": y}))
            }
            Command::Back => {
                let back = keys::TABLE
                    .iter()
                    .find(|mapping| mapping.key_code == KEYCODE_BACK)
                    .map_or(158, |mapping| mapping.evdev);
                self.press(back, false)?;
                Ok(json!({"key": "BACK"}))
            }
            Command::Key(name) => self.key(&name),
            Command::Wait(wait) => self.wait(
                session,
                &wait.condition,
                Duration::from_millis(u64::from(wait.timeout_ms)),
            ),
            other => self.app.handle(other),
        }
    }

    fn ended(&self, session: &Session) {
        if !session.agent {
            return;
        }
        let last = {
            let mut agents = lock(&self.agents);
            *agents = agents.saturating_sub(1);
            *agents == 0
        };
        if last {
            let _ = self.app.handle(Command::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keys::Levels;

    #[test]
    fn typing_plans_shift_for_capitals_and_commits_the_rest() {
        let charmap = Charmap::from_levels(|evdev| match evdev {
            30 => Levels {
                base: Some('a'),
                shift: Some('A'),
                ralt: Some('æ'),
                ..Levels::default()
            },
            _ => Levels::default(),
        });
        assert_eq!(
            plan(&charmap, "aAæ€\u{1F600}a"),
            vec![
                Step::Key {
                    evdev: 30,
                    shift: false
                },
                Step::Key {
                    evdev: 30,
                    shift: true
                },
                Step::Commit("æ€\u{1F600}".into()),
                Step::Key {
                    evdev: 30,
                    shift: false
                },
            ]
        );
    }
}
