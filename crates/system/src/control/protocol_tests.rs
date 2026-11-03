//! The control protocol: requests, answers and lines, refused with a reason and never a panic.

use std::io::BufReader;

use serde_json::json;

use super::protocol::{self, Command, Failure, Intent, MAX_LINE};

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
        failure(br#"{"v":1,"cmd":"screenshot"}"#).code,
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
