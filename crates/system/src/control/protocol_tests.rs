//! The control protocol: requests, answers and lines, refused with a reason and never a panic.

use std::io::BufReader;

use serde_json::json;

use super::protocol::{
    self, Command, Condition, Failure, Intent, MAX_LINE, Place, Screenshot, Scroll, Typing, Ui,
    Wait,
};

fn failure(line: &[u8]) -> Failure {
    protocol::parse_request(line).unwrap_err().1
}

// Requests

#[test]
fn a_request_names_its_version_command_and_id() {
    let request = protocol::parse_request(br#"{"v":1,"id":7,"cmd":"status"}"#).unwrap();
    assert_eq!(request.id, Some(json!(7)));
    assert_eq!(request.command, Command::Status);
    let request = protocol::parse_request(br#"{"v":1,"cmd":"stop"}"#).unwrap();
    assert_eq!(request.id, None);
    assert_eq!(request.command, Command::Stop);
}

#[test]
fn a_start_carries_an_intent() {
    let line = br#"{"v":1,"id":"a","cmd":"start","intent":{"activity":".Detail","action":"android.intent.action.VIEW","data":"https://example.com/x","extras":{"k":"v","n":"1"},"flags":603979776}}"#;
    let request = protocol::parse_request(line).unwrap();
    let Command::Start(intent) = request.command else {
        panic!("not a start: {:?}", request.command);
    };
    assert_eq!(intent.activity.as_deref(), Some(".Detail"));
    assert_eq!(intent.action.as_deref(), Some("android.intent.action.VIEW"));
    assert_eq!(intent.data.as_deref(), Some("https://example.com/x"));
    assert_eq!(
        intent.extras,
        vec![
            ("k".to_string(), "v".to_string()),
            ("n".to_string(), "1".to_string())
        ]
    );
    assert_eq!(intent.flags, 0x2400_0000);
    // An intent is optional for start, required for intent.
    assert_eq!(
        protocol::parse_request(br#"{"v":1,"cmd":"start"}"#)
            .unwrap()
            .command,
        Command::Start(Intent::default())
    );
    assert_eq!(
        failure(br#"{"v":1,"cmd":"intent"}"#).code,
        protocol::BAD_REQUEST
    );
}

#[test]
fn an_intent_survives_its_round_trip() {
    let intent = Intent {
        activity: Some("com.example.Main".into()),
        action: Some("a.b".into()),
        data: Some("x:y".into()),
        extras: vec![("k".into(), "line\nbreak \"quoted\"".into())],
        flags: 4,
    };
    let line = protocol::request_line(Some(&json!(3)), &Command::Intent(intent.clone()));
    assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
    assert_eq!(
        protocol::parse_request(line.as_bytes()).unwrap().command,
        Command::Intent(intent.clone())
    );
    assert_eq!(Intent::from_json(&intent.to_json()).unwrap(), intent);
}

#[test]
fn bad_json_is_refused_with_a_reason() {
    for line in [
        &b"{"[..],
        b"",
        b"not json",
        b"[1,2]",
        b"\"text\"",
        b"null",
        b"{\"v\":1,\"cmd\":\"status\"} extra",
        b"\xff\xfe",
    ] {
        let failure = failure(line);
        assert_eq!(failure.code, protocol::BAD_REQUEST, "{line:?}");
        assert!(!failure.message.is_empty());
    }
}

#[test]
fn unknown_versions_are_refused_naming_the_supported_one() {
    for line in [
        &br#"{"v":2,"cmd":"status"}"#[..],
        br#"{"cmd":"status"}"#,
        br#"{"v":"1","cmd":"status"}"#,
        br#"{"v":0,"cmd":"status"}"#,
    ] {
        let failure = failure(line);
        assert_eq!(failure.code, protocol::UNSUPPORTED_VERSION, "{line:?}");
        assert!(failure.message.contains("version 1"), "{}", failure.message);
    }
}

#[test]
fn unknown_commands_and_fields_are_refused() {
    assert_eq!(
        failure(br#"{"v":1,"cmd":"teleport"}"#).code,
        protocol::UNKNOWN_COMMAND
    );
    assert_eq!(failure(br#"{"v":1,"cmd":5}"#).code, protocol::BAD_REQUEST);
    assert_eq!(failure(br#"{"v":1}"#).code, protocol::BAD_REQUEST);
    assert_eq!(
        failure(br#"{"v":1,"cmd":"status","extra":1}"#).code,
        protocol::BAD_REQUEST
    );
    assert_eq!(
        failure(br#"{"v":1,"cmd":"start","intent":{"class":"x"}}"#).code,
        protocol::BAD_REQUEST
    );
    // The id is echoed even when the request is refused, so a client can match the answer.
    let (id, _) = protocol::parse_request(br#"{"v":1,"id":9,"cmd":"nope"}"#).unwrap_err();
    assert_eq!(id, Some(json!(9)));
}

#[test]
fn intent_fields_are_checked() {
    let refused = |intent: &str| {
        failure(format!(r#"{{"v":1,"cmd":"intent","intent":{intent}}}"#).as_bytes()).code
    };
    for intent in [
        r#"[]"#,
        r#"{"activity":"com/evil"}"#,
        r#"{"activity":""}"#,
        r#"{"activity":"a b"}"#,
        r#"{"action":7}"#,
        r#"{"action":"a\u0000b"}"#,
        r#"{"data":"x\ny"}"#,
        r#"{"extras":{"k":1}}"#,
        r#"{"extras":{"":"v"}}"#,
        r#"{"extras":[]}"#,
        r#"{"flags":-1}"#,
        r#"{"flags":4294967296}"#,
        r#"{"flags":1.5}"#,
    ] {
        assert_eq!(refused(intent), protocol::BAD_REQUEST, "{intent}");
    }
    let long = "a".repeat(protocol::MAX_NAME + 1);
    assert_eq!(
        refused(&format!(r#"{{"action":"{long}"}}"#)),
        protocol::BAD_REQUEST
    );
    let many: Vec<String> = (0..=protocol::MAX_EXTRAS)
        .map(|n| format!(r#""k{n}":"v""#))
        .collect();
    assert_eq!(
        refused(&format!(r#"{{"extras":{{{}}}}}"#, many.join(","))),
        protocol::BAD_REQUEST
    );
}

#[test]
fn deep_nesting_is_refused_without_overflowing_the_stack() {
    let deep = format!(
        r#"{{"v":1,"cmd":"status","id":{}1{}}}"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    assert_eq!(failure(deep.as_bytes()).code, protocol::BAD_REQUEST);
}

#[test]
fn ids_are_numbers_or_short_strings() {
    assert_eq!(
        failure(br#"{"v":1,"id":{},"cmd":"status"}"#).code,
        protocol::BAD_REQUEST
    );
    assert_eq!(
        failure(br#"{"v":1,"id":-1,"cmd":"status"}"#).code,
        protocol::BAD_REQUEST
    );
    let long = format!(r#"{{"v":1,"id":"{}","cmd":"status"}}"#, "x".repeat(65));
    assert_eq!(failure(long.as_bytes()).code, protocol::BAD_REQUEST);
}

// Lines

#[test]
fn lines_longer_than_the_limit_are_refused() {
    let mut input = vec![b'x'; MAX_LINE + 10];
    input.push(b'\n');
    let mut reader = BufReader::new(&input[..]);
    assert!(matches!(
        protocol::read_line(&mut reader),
        Err(protocol::LineError::TooLong)
    ));

    let mut fits = vec![b'x'; MAX_LINE - 1];
    fits.push(b'\n');
    fits.extend_from_slice(b"next\n");
    let mut reader = BufReader::new(&fits[..]);
    assert_eq!(
        protocol::read_line(&mut reader).unwrap().unwrap().len(),
        MAX_LINE - 1
    );
    assert_eq!(protocol::read_line(&mut reader).unwrap().unwrap(), b"next");
    assert!(protocol::read_line(&mut reader).unwrap().is_none());
}

#[test]
fn a_line_without_its_newline_at_the_end_still_counts() {
    let mut reader = BufReader::new(&b"last"[..]);
    assert_eq!(protocol::read_line(&mut reader).unwrap().unwrap(), b"last");
    assert!(protocol::read_line(&mut reader).unwrap().is_none());
}

// Responses

#[test]
fn responses_are_read_back_and_hostile_ones_refused() {
    let ok = protocol::response_ok(Some(&json!(1)), json!({"pid": 5}));
    assert_eq!(
        protocol::parse_response(ok.trim_end().as_bytes()).unwrap(),
        Ok(json!({"pid": 5}))
    );
    let failure = Failure::new(protocol::UNKNOWN_COMMAND, "no such command");
    let error = protocol::response_error(None, &failure);
    assert_eq!(
        protocol::parse_response(error.trim_end().as_bytes()).unwrap(),
        Err(failure)
    );
    for hostile in [
        &b"{"[..],
        b"[]",
        br#"{"v":2,"ok":true,"result":{}}"#,
        br#"{"v":1,"ok":"yes"}"#,
        br#"{"v":1,"ok":false,"error":{"code":1}}"#,
    ] {
        assert!(protocol::parse_response(hostile).is_err(), "{hostile:?}");
    }
}

// Agent commands

fn command(line: &str) -> Command {
    protocol::parse_request(line.as_bytes())
        .unwrap_or_else(|(_, failure)| panic!("{line}: {failure:?}"))
        .command
}

#[test]
fn agent_commands_take_their_arguments_with_defaults() {
    assert_eq!(
        command(r#"{"v":1,"cmd":"ui"}"#),
        Command::Ui(Ui {
            max_nodes: protocol::DEFAULT_NODES,
            max_depth: protocol::DEFAULT_DEPTH,
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"ui","max_nodes":10,"max_depth":3}"#),
        Command::Ui(Ui {
            max_nodes: 10,
            max_depth: 3
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"screenshot","window":1,"max_size":640}"#),
        Command::Screenshot(Screenshot {
            window: Some(1),
            max_size: 640
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"screenshot"}"#),
        Command::Screenshot(Screenshot {
            window: None,
            max_size: protocol::DEFAULT_SHOT_SIZE
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"tap","x":10,"y":20}"#),
        Command::Tap(Place::Point { x: 10, y: 20 })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"tap","node":4,"snapshot":2}"#),
        Command::Tap(Place::Node {
            node: 4,
            snapshot: Some(2)
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"type","text":"Hi\nthere"}"#),
        Command::Type(Typing {
            text: "Hi\nthere".into(),
            node: None
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"type","text":"x","node":3}"#),
        Command::Type(Typing {
            text: "x".into(),
            node: Some(Place::Node {
                node: 3,
                snapshot: None
            })
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"scroll","x":5,"y":6,"dy":-3}"#),
        Command::Scroll(Scroll {
            at: Place::Point { x: 5, y: 6 },
            dx: 0,
            dy: -3
        })
    );
    assert_eq!(command(r#"{"v":1,"cmd":"back"}"#), Command::Back);
    assert_eq!(
        command(r#"{"v":1,"cmd":"key","key":"ENTER"}"#),
        Command::Key("ENTER".into())
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"wait","text":"Done","timeout_ms":500}"#),
        Command::Wait(Wait {
            condition: Condition::Text("Done".into()),
            timeout_ms: 500
        })
    );
    assert_eq!(
        command(r#"{"v":1,"cmd":"wait","idle":true}"#),
        Command::Wait(Wait {
            condition: Condition::Idle,
            timeout_ms: protocol::DEFAULT_WAIT_MS
        })
    );
}

#[test]
fn agent_commands_survive_their_round_trip() {
    for sent in [
        Command::Ui(Ui {
            max_nodes: 7,
            max_depth: 2,
        }),
        Command::Screenshot(Screenshot {
            window: Some(0),
            max_size: 100,
        }),
        Command::Tap(Place::Node {
            node: 9,
            snapshot: Some(4),
        }),
        Command::Type(Typing {
            text: "a \"quoted\"\tline\n".into(),
            node: Some(Place::Point { x: 1, y: 2 }),
        }),
        Command::Scroll(Scroll {
            at: Place::Point { x: 0, y: 0 },
            dx: 2,
            dy: 0,
        }),
        Command::Back,
        Command::Key("DPAD_DOWN".into()),
        Command::Wait(Wait {
            condition: Condition::Idle,
            timeout_ms: 1,
        }),
    ] {
        let line = protocol::request_line(None, &sent);
        assert!(!line[..line.len() - 1].contains('\n'), "{line}");
        assert_eq!(command(line.trim_end()), sent, "{line}");
    }
}

#[test]
fn agent_arguments_are_bounded_and_checked() {
    for line in [
        r#"{"v":1,"cmd":"ui","max_nodes":0}"#,
        r#"{"v":1,"cmd":"ui","max_nodes":5001}"#,
        r#"{"v":1,"cmd":"ui","max_depth":129}"#,
        r#"{"v":1,"cmd":"ui","max_depth":"3"}"#,
        r#"{"v":1,"cmd":"screenshot","max_size":15}"#,
        r#"{"v":1,"cmd":"screenshot","max_size":4097}"#,
        r#"{"v":1,"cmd":"screenshot","window":64}"#,
        r#"{"v":1,"cmd":"tap"}"#,
        r#"{"v":1,"cmd":"tap","x":1}"#,
        r#"{"v":1,"cmd":"tap","x":-1,"y":0}"#,
        r#"{"v":1,"cmd":"tap","x":16384,"y":0}"#,
        r#"{"v":1,"cmd":"tap","x":1.5,"y":0}"#,
        r#"{"v":1,"cmd":"tap","node":0}"#,
        r#"{"v":1,"cmd":"tap","node":1,"x":1,"y":1}"#,
        r#"{"v":1,"cmd":"tap","snapshot":1,"x":1,"y":1}"#,
        r#"{"v":1,"cmd":"type"}"#,
        r#"{"v":1,"cmd":"type","text":""}"#,
        r#"{"v":1,"cmd":"type","text":"a\u0000b"}"#,
        r#"{"v":1,"cmd":"type","text":"\u001b[2J"}"#,
        r#"{"v":1,"cmd":"scroll","x":1,"y":1}"#,
        r#"{"v":1,"cmd":"scroll","x":1,"y":1,"dy":51}"#,
        r#"{"v":1,"cmd":"scroll","dy":1}"#,
        r#"{"v":1,"cmd":"key"}"#,
        r#"{"v":1,"cmd":"key","key":"enter; rm -rf /"}"#,
        r#"{"v":1,"cmd":"key","key":""}"#,
        r#"{"v":1,"cmd":"wait"}"#,
        r#"{"v":1,"cmd":"wait","text":"a","idle":true}"#,
        r#"{"v":1,"cmd":"wait","idle":false}"#,
        r#"{"v":1,"cmd":"wait","text":"a","timeout_ms":0}"#,
        r#"{"v":1,"cmd":"wait","text":"a","timeout_ms":60001}"#,
        r#"{"v":1,"cmd":"back","x":1}"#,
        r#"{"v":1,"cmd":"status","text":"a"}"#,
        r#"{"v":1,"cmd":"ui","intent":{}}"#,
    ] {
        assert_eq!(
            failure(line.as_bytes()).code,
            protocol::BAD_REQUEST,
            "{line}"
        );
    }
    let long = "a".repeat(protocol::MAX_TEXT + 1);
    let line = format!(r#"{{"v":1,"cmd":"type","text":"{long}"}}"#);
    assert_eq!(failure(line.as_bytes()).code, protocol::BAD_REQUEST);
    let fits = "a".repeat(protocol::MAX_TEXT);
    let line = format!(r#"{{"v":1,"cmd":"type","text":"{fits}"}}"#);
    assert!(matches!(command(&line), Command::Type(_)));
}

#[test]
fn hello_lists_the_agent_commands_and_internal_ones_stay_hidden() {
    for name in [
        "ui",
        "screenshot",
        "tap",
        "type",
        "scroll",
        "back",
        "key",
        "wait",
    ] {
        assert!(Command::NAMES.contains(&name), "{name}");
    }
    // Commands the app's agent support sends to its own main thread are not part of the protocol.
    for name in ["commit", "release"] {
        assert!(!Command::NAMES.contains(&name), "{name}");
        let line = format!(r#"{{"v":1,"cmd":"{name}"}}"#);
        assert_eq!(failure(line.as_bytes()).code, protocol::UNKNOWN_COMMAND);
    }
}
