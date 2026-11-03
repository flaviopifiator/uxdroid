//! Agent commands through the agent's public interface: a scripted app answers what the main thread
//! would, and a recording seat stands in for the compositor's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::agent::{Agent, Input, Seat, WindowFrame, png};
use super::protocol::{
    self, Command, Condition, Failure, Place, Screenshot, Scroll, Typing, Ui, Wait,
};
use super::server::{Handler, Session};
use crate::input::charmap::Charmap;
use crate::input::keys::Levels;

/// What the main thread answers: each `ui` takes the next snapshot, the last one repeating.
struct App {
    snapshots: Mutex<Vec<Value>>,
    received: Mutex<Vec<Command>>,
}

impl Handler for App {
    fn handle(&self, command: Command) -> Result<Value, Failure> {
        self.received.lock().unwrap().push(command.clone());
        match command {
            Command::Ui(_) => {
                let mut snapshots = self.snapshots.lock().unwrap();
                let next = if snapshots.len() > 1 {
                    snapshots.remove(0)
                } else {
                    snapshots[0].clone()
                };
                // A null snapshot is a main thread that could not answer this time.
                if next.is_null() {
                    return Err(Failure::new(protocol::FAILED, "the main thread is busy"));
                }
                Ok(next)
            }
            Command::Commit(text) => Ok(json!({"committed": text.chars().count()})),
            Command::Release => Ok(json!({})),
            other => Ok(json!({"forwarded": other.name()})),
        }
    }
}

#[derive(Default)]
struct FakeSeat {
    display: Option<(i32, i32)>,
    inputs: Mutex<Vec<Input>>,
    presented: AtomicU64,
    /// Frames keep coming while true.
    busy: bool,
    frames: Vec<WindowFrame>,
    /// Which sends, counted from 0, fail without reaching the app.
    failing: Vec<usize>,
    sends: AtomicU64,
}

impl Seat for FakeSeat {
    fn display(&self) -> Option<(i32, i32)> {
        self.display
    }
    fn send(&self, input: Input) -> Result<(), String> {
        let index = self.sends.fetch_add(1, Ordering::Relaxed) as usize;
        if self.failing.contains(&index) {
            return Err("the compositor's seat is gone".into());
        }
        self.inputs.lock().unwrap().push(input);
        Ok(())
    }
    fn presented(&self) -> u64 {
        if self.busy {
            self.presented.fetch_add(1, Ordering::Relaxed)
        } else {
            self.presented.load(Ordering::Relaxed)
        }
    }
    fn frames(&self) -> Vec<WindowFrame> {
        self.frames.clone()
    }
    fn charmap(&self) -> Arc<Charmap> {
        Arc::new(us())
    }
}

/// The keys these tests type, as a US layout has them.
fn us() -> Charmap {
    let letter = |c: char| Levels {
        base: Some(c),
        shift: Some(c.to_ascii_uppercase()),
        caps: Some(c.to_ascii_uppercase()),
        shift_caps: Some(c),
        ..Levels::default()
    };
    Charmap::from_levels(|evdev| match evdev {
        35 => letter('h'),
        23 => letter('i'),
        2 => Levels {
            base: Some('1'),
            shift: Some('!'),
            ..Levels::default()
        },
        _ => Levels::default(),
    })
}

fn tree(snapshot: u64, text: &str) -> Value {
    json!({
        "snapshot": snapshot,
        "windows": [{"title": "main", "root": {
            "id": 1, "class": "android.widget.FrameLayout", "bounds": [0, 0, 400, 700], "visible": true,
            "children": [
                {"id": 2, "class": "android.widget.Button", "text": "Tap", "bounds": [10, 20, 110, 60],
                 "visible": true, "clickable": true},
                {"id": 3, "class": "android.widget.EditText", "text": text, "hint": "Type here",
                 "bounds": [10, 80, 390, 120], "visible": true, "editable": true},
                {"id": 4, "class": "android.view.View", "text": "hidden", "bounds": [0, 0, 0, 0]},
            ]
        }}]
    })
}

fn make(seat: FakeSeat, snapshots: Vec<Value>) -> (Agent, Arc<App>, Arc<FakeSeat>) {
    let app = Arc::new(App {
        snapshots: Mutex::new(snapshots),
        received: Mutex::new(Vec::new()),
    });
    let seat = Arc::new(seat);
    (Agent::new(app.clone(), seat.clone()), app, seat)
}

fn screen() -> FakeSeat {
    FakeSeat {
        display: Some((400, 700)),
        ..FakeSeat::default()
    }
}

fn run(agent: &Agent, command: Command) -> Result<Value, Failure> {
    agent.handle_in(&mut Session::default(), command)
}

fn ui() -> Command {
    Command::Ui(Ui {
        max_nodes: 100,
        max_depth: 10,
    })
}

fn code(result: Result<Value, Failure>) -> String {
    result.expect_err("refused").code
}

fn taps(x: i32, y: i32) -> Vec<Input> {
    vec![Input::TouchDown { x, y }, Input::TouchUp]
}

fn key(evdev: u32, shift: bool) -> Vec<Input> {
    vec![
        Input::Key {
            evdev,
            pressed: true,
            shift,
        },
        Input::Key {
            evdev,
            pressed: false,
            shift,
        },
    ]
}

// Acting at a point or on a node

#[test]
fn a_tap_at_a_point_is_a_finger_down_and_up_on_the_display() {
    let (agent, _, seat) = make(screen(), vec![tree(1, "")]);
    let result = run(&agent, Command::Tap(Place::Point { x: 30, y: 40 })).unwrap();
    assert_eq!(result, json!({"x": 30, "y": 40}));
    assert_eq!(*seat.inputs.lock().unwrap(), taps(30, 40));
    // Outside the display, or with no window at all, nothing is sent.
    let outside = run(&agent, Command::Tap(Place::Point { x: 400, y: 10 }));
    assert_eq!(code(outside), protocol::BAD_REQUEST);
    let (blind, _, seat) = make(FakeSeat::default(), vec![tree(1, "")]);
    let none = run(&blind, Command::Tap(Place::Point { x: 1, y: 1 }));
    assert_eq!(code(none), protocol::NO_WINDOW);
    assert!(seat.inputs.lock().unwrap().is_empty());
}

#[test]
fn a_tap_on_a_node_hits_the_center_of_its_bounds_in_the_last_snapshot() {
    let (agent, _, seat) = make(screen(), vec![tree(7, "")]);
    let node = |node, snapshot| Command::Tap(Place::Node { node, snapshot });
    // No snapshot yet: no node is known.
    assert_eq!(code(run(&agent, node(2, None))), protocol::NO_SUCH_NODE);
    run(&agent, ui()).unwrap();
    let result = run(&agent, node(2, Some(7))).unwrap();
    assert_eq!(result, json!({"x": 60, "y": 40, "node": 2}));
    assert_eq!(*seat.inputs.lock().unwrap(), taps(60, 40));
    assert_eq!(
        code(run(&agent, node(2, Some(6)))),
        protocol::STALE_SNAPSHOT
    );
    assert_eq!(code(run(&agent, node(99, None))), protocol::NO_SUCH_NODE);
    // A node with no area cannot be tapped.
    assert_eq!(code(run(&agent, node(4, None))), protocol::NO_SUCH_NODE);
}

#[test]
fn hostile_snapshots_never_panic_and_hold_no_node() {
    for answer in [
        json!(null),
        json!({"snapshot": "x", "windows": 3}),
        json!({"snapshot": 1, "windows": [{"root": {"id": -1, "bounds": [1]}}]}),
        json!({"snapshot": 1, "windows": [{"root": {"id": 2, "bounds": [9, 9, 1, 1]}}]}),
    ] {
        let (agent, _, _) = make(screen(), vec![answer]);
        let _ = run(&agent, ui());
        let tap = run(
            &agent,
            Command::Tap(Place::Node {
                node: 2,
                snapshot: None,
            }),
        );
        assert_eq!(code(tap), protocol::NO_SUCH_NODE);
    }
}

// Typing and keys

#[test]
fn typed_text_becomes_key_presses_of_the_seat_keymap() {
    let (agent, app, seat) = make(screen(), vec![tree(1, "")]);
    let typing = |text: &str, node| {
        Command::Type(Typing {
            text: text.into(),
            node,
        })
    };
    let result = run(&agent, typing("Hi!\n", None)).unwrap();
    assert_eq!(result, json!({"typed": 4, "keys": 4, "committed": 0}));
    let expected: Vec<Input> =
        [key(35, true), key(23, false), key(2, true), key(28, false)].concat();
    assert_eq!(*seat.inputs.lock().unwrap(), expected);
    // What the keymap cannot type is committed through the focused view, in order.
    seat.inputs.lock().unwrap().clear();
    let result = run(&agent, typing("hé€i", None)).unwrap();
    assert_eq!(result, json!({"typed": 4, "keys": 2, "committed": 2}));
    assert_eq!(
        *seat.inputs.lock().unwrap(),
        [key(35, false), key(23, false)].concat()
    );
    assert!(
        app.received
            .lock()
            .unwrap()
            .contains(&Command::Commit("é€".into()))
    );
    // A node is tapped first, which focuses it as a user's tap would.
    run(&agent, ui()).unwrap();
    seat.inputs.lock().unwrap().clear();
    let node = Some(Place::Node {
        node: 3,
        snapshot: None,
    });
    run(&agent, typing("h", node)).unwrap();
    assert_eq!(
        *seat.inputs.lock().unwrap(),
        [taps(200, 100), key(35, false)].concat()
    );
}

#[test]
fn keys_are_named_as_android_names_them() {
    let (agent, _, seat) = make(screen(), vec![tree(1, "")]);
    run(&agent, Command::Key("ENTER".into())).unwrap();
    run(&agent, Command::Back).unwrap();
    run(&agent, Command::Key("DPAD_DOWN".into())).unwrap();
    assert_eq!(
        *seat.inputs.lock().unwrap(),
        [key(28, false), key(158, false), key(108, false)].concat()
    );
    let unknown = run(&agent, Command::Key("WARP_DRIVE".into())).unwrap_err();
    assert_eq!(unknown.code, protocol::BAD_REQUEST);
    assert!(
        unknown.message.contains("WARP_DRIVE"),
        "{}",
        unknown.message
    );
}

#[test]
fn a_scroll_is_wheel_steps_at_a_point() {
    let (agent, _, seat) = make(screen(), vec![tree(1, "")]);
    run(&agent, ui()).unwrap();
    let scroll = Scroll {
        at: Place::Node {
            node: 1,
            snapshot: None,
        },
        dx: 0,
        dy: 3,
    };
    assert_eq!(
        run(&agent, Command::Scroll(scroll)).unwrap(),
        json!({"x": 200, "y": 350})
    );
    assert_eq!(
        *seat.inputs.lock().unwrap(),
        vec![Input::Wheel {
            x: 200,
            y: 350,
            dx: 0,
            dy: 3
        }]
    );
}

// Waiting

#[test]
fn wait_returns_the_node_once_its_text_shows() {
    let (agent, _, _) = make(
        screen(),
        vec![tree(1, ""), tree(2, ""), tree(3, "Hi there")],
    );
    let wait = Command::Wait(Wait {
        condition: Condition::Text("there".into()),
        timeout_ms: 5000,
    });
    let result = run(&agent, wait).unwrap();
    assert_eq!(result["snapshot"], json!(3));
    assert_eq!(result["node"]["id"], json!(3));
    assert_eq!(result["node"]["text"], json!("Hi there"));
    assert!(result["node"].get("children").is_none());
    // The snapshot it found the node in is the last one, so the node can be tapped at once.
    run(
        &agent,
        Command::Tap(Place::Node {
            node: 3,
            snapshot: Some(3),
        }),
    )
    .unwrap();
}

#[test]
fn wait_runs_out_of_time_with_a_timeout() {
    let (agent, _, _) = make(screen(), vec![tree(1, "")]);
    let started = Instant::now();
    let wait = Command::Wait(Wait {
        // Hidden nodes do not count.
        condition: Condition::Text("hidden".into()),
        timeout_ms: 300,
    });
    assert_eq!(code(run(&agent, wait)), protocol::TIMEOUT);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(3),
        "{took:?}"
    );
}

#[test]
fn wait_for_idle_needs_a_quiet_display() {
    let idle = Command::Wait(Wait {
        condition: Condition::Idle,
        timeout_ms: 3000,
    });
    let (agent, _, _) = make(screen(), vec![tree(1, "")]);
    assert_eq!(run(&agent, idle.clone()).unwrap()["idle"], json!(true));
    let busy = FakeSeat {
        busy: true,
        ..screen()
    };
    let (agent, _, _) = make(busy, vec![tree(1, "")]);
    let short = Command::Wait(Wait {
        condition: Condition::Idle,
        timeout_ms: 700,
    });
    assert_eq!(code(run(&agent, short)), protocol::TIMEOUT);
}

// Accessibility is on only while an agent that asked for it is connected

#[test]
fn the_last_agent_leaving_releases_accessibility() {
    let (agent, app, _) = make(screen(), vec![tree(1, "")]);
    let releases = || {
        app.received
            .lock()
            .unwrap()
            .iter()
            .filter(|command| **command == Command::Release)
            .count()
    };
    let (mut first, mut second, mut plain) =
        (Session::default(), Session::default(), Session::default());
    agent.handle_in(&mut first, ui()).unwrap();
    agent.handle_in(&mut second, ui()).unwrap();
    agent.handle_in(&mut plain, Command::Status).unwrap();
    agent.ended(&plain);
    agent.ended(&first);
    assert_eq!(releases(), 0, "an agent is still connected");
    agent.ended(&second);
    assert_eq!(releases(), 1);
}

// Screenshots

fn frame(x: i32, y: i32, width: i32, height: i32, color: u32) -> WindowFrame {
    WindowFrame {
        x,
        y,
        width,
        height,
        pixels: vec![color; (width * height) as usize],
    }
}

fn shot(agent: &Agent, window: Option<u32>, max_size: u32) -> Result<(Value, png::Image), Failure> {
    let result = run(agent, Command::Screenshot(Screenshot { window, max_size }))?;
    let data = png::base64_decode(result["png"].as_str().unwrap()).unwrap();
    Ok((result, png::decode(&data).unwrap()))
}

#[test]
fn a_screenshot_composes_the_windows_bottom_first() {
    let seat = FakeSeat {
        display: Some((8, 6)),
        frames: vec![
            frame(0, 0, 8, 6, 0xFF11_2233),
            frame(2, 1, 3, 2, 0xFFAA_BBCC),
        ],
        ..FakeSeat::default()
    };
    let (agent, _, _) = make(seat, vec![tree(1, "")]);
    let (result, image) = shot(&agent, None, 2048).unwrap();
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(8), json!(6))
    );
    assert_eq!((image.width, image.height), (8, 6));
    assert_eq!(image.pixel(0, 0), [0x11, 0x22, 0x33]);
    assert_eq!(image.pixel(2, 1), [0xAA, 0xBB, 0xCC]);
    assert_eq!(image.pixel(4, 2), [0xAA, 0xBB, 0xCC]);
    assert_eq!(image.pixel(5, 2), [0x11, 0x22, 0x33]);
    // One window alone, by its place in the stack.
    let (_, image) = shot(&agent, Some(1), 2048).unwrap();
    assert_eq!((image.width, image.height), (3, 2));
    assert_eq!(
        code(shot(&agent, Some(2), 2048).map(|(v, _)| v)),
        protocol::NO_WINDOW
    );
    // A size cap scales the longest side down.
    let (_, image) = shot(&agent, None, 16).unwrap();
    assert_eq!((image.width, image.height), (8, 6));
}

#[test]
fn a_large_display_is_scaled_to_the_cap() {
    let seat = FakeSeat {
        display: Some((400, 700)),
        frames: vec![frame(0, 0, 400, 700, 0xFF00_FF00)],
        ..FakeSeat::default()
    };
    let (agent, _, _) = make(seat, vec![tree(1, "")]);
    let (result, image) = shot(&agent, None, 100).unwrap();
    assert_eq!((image.width, image.height), (57, 100));
    assert_eq!(result["scale"], json!(7));
    assert_eq!(image.pixel(56, 99), [0, 0xFF, 0]);
    let (blind, _, _) = make(FakeSeat::default(), vec![tree(1, "")]);
    assert_eq!(
        code(shot(&blind, None, 100).map(|(v, _)| v)),
        protocol::NO_WINDOW
    );
}

#[test]
fn other_commands_go_to_the_app() {
    let (agent, app, _) = make(screen(), vec![tree(1, "")]);
    assert_eq!(
        run(&agent, Command::Status).unwrap(),
        json!({"forwarded": "status"})
    );
    assert_eq!(*app.received.lock().unwrap(), vec![Command::Status]);
}

// Failures on the way

#[test]
fn a_tap_whose_finger_up_fails_once_still_lifts_the_finger() {
    let seat = FakeSeat {
        failing: vec![1],
        ..screen()
    };
    let (agent, _, seat) = make(seat, vec![tree(1, "")]);
    let tapped = run(&agent, Command::Tap(Place::Point { x: 30, y: 40 }));
    assert_eq!(tapped.unwrap(), json!({"x": 30, "y": 40}));
    assert_eq!(*seat.inputs.lock().unwrap(), taps(30, 40));
}

#[test]
fn typing_that_fails_part_way_says_how_much_was_typed() {
    let seat = FakeSeat {
        // The second character's key down.
        failing: vec![2, 3, 4],
        ..screen()
    };
    let (agent, _, seat) = make(seat, vec![tree(1, "")]);
    let typing = Command::Type(Typing {
        text: "hi".into(),
        node: None,
    });
    let failure = run(&agent, typing).expect_err("the seat failed");
    assert_eq!(failure.code, protocol::FAILED);
    assert!(
        failure.message.contains("after 1 of 2 characters"),
        "{}",
        failure.message
    );
    assert_eq!(*seat.inputs.lock().unwrap(), key(35, false));
}

#[test]
fn wait_outlasts_a_ui_snapshot_that_failed_once() {
    let (agent, _, _) = make(screen(), vec![Value::Null, tree(2, "Hi there")]);
    let wait = Command::Wait(Wait {
        condition: Condition::Text("there".into()),
        timeout_ms: 5000,
    });
    assert_eq!(run(&agent, wait).unwrap()["snapshot"], json!(2));
    // A failure that lasts is reported when the time runs out.
    let (agent, _, _) = make(screen(), vec![Value::Null]);
    let wait = Command::Wait(Wait {
        condition: Condition::Text("there".into()),
        timeout_ms: 200,
    });
    let failure = run(&agent, wait).expect_err("never answered");
    assert_eq!(failure.code, protocol::TIMEOUT);
    assert!(failure.message.contains("busy"), "{}", failure.message);
}

#[test]
fn a_screenshot_of_no_size_or_of_an_empty_display_never_panics() {
    let seat = FakeSeat {
        display: Some((8, 6)),
        frames: vec![frame(0, 0, 8, 6, 0xFF11_2233)],
        ..FakeSeat::default()
    };
    let (agent, _, _) = make(seat, vec![tree(1, "")]);
    let (_, image) = shot(&agent, None, 0).unwrap();
    assert_eq!((image.width, image.height), (1, 1));
    let seat = FakeSeat {
        display: Some((0, 0)),
        frames: vec![frame(0, 0, 0, 0, 0)],
        ..FakeSeat::default()
    };
    let (agent, _, _) = make(seat, vec![tree(1, "")]);
    assert_eq!(
        code(shot(&agent, None, 100).map(|(v, _)| v)),
        protocol::NO_WINDOW
    );
    assert_eq!(
        code(shot(&agent, Some(0), 100).map(|(v, _)| v)),
        protocol::NO_WINDOW
    );
}
