//! The Google Play source through the commands, against a fake Play and an F-Droid repository made
//! by the testkit and served on this machine, so no test needs the network or a Google account.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use uxdroid_testkit::fdroid::{self, Version};
use uxdroid_testkit::play::{AAS_TOKEN, Answer, FakePlay, OAUTH_TOKEN, PlayApp, Proto};
use uxdroid_testkit::{Apk, Key, app_manifest, dex, split_manifest};

const FIRST: u64 = 1_793_000_000_000;
const UPDATED: u64 = 1_793_273_970_519; // 2026-10-29
const CHAT: &str = "com.example.chat";
const NOTES: &str = "com.example.notes";

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("play-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn command(data: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uxdroid"));
    command
        .args(args)
        .arg("--data")
        .arg(data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "/nonexistent")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES");
    for proxy in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        command.env_remove(proxy);
    }
    command
}

fn uxdroid(data: &Path, args: &[&str]) -> Output {
    command(data, args).stdin(Stdio::null()).output().unwrap()
}

/// Runs uxdroid with `input` on its standard input.
fn uxdroid_with(data: &Path, args: &[&str], input: &str) -> Output {
    let mut child = command(data, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn ok(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

/// The error a command failed with: exit 1, nothing on standard output.
fn refused(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stderr)
}

fn apk(package: &'static str, code: u32, name: &'static str, key: &Key) -> Vec<u8> {
    Apk::new()
        .manifest(&app_manifest(package, code, name))
        .file("classes.dex", &dex(1, 1))
        .signed_bytes(key)
}

fn split(package: &'static str, code: u32, name: &'static str) -> Apk {
    Apk::new().manifest(&split_manifest(package, code, name, None, false))
}

/// Chat on the fake Play: a base and the splits Play serves an x86_64 device at 160 dpi.
fn chat(code: u32, name: &'static str, key: &Key) -> PlayApp {
    PlayApp::new(
        CHAT,
        "Chat Messenger",
        u64::from(code),
        name,
        apk(CHAT, code, name, key),
        &key.fingerprint(),
    )
    .split(
        "config.x86_64",
        split(CHAT, code, "config.x86_64")
            .native_lib("x86_64", "libchat.so")
            .signed_bytes(key),
    )
    .split(
        "config.mdpi",
        split(CHAT, code, "config.mdpi").signed_bytes(key),
    )
}

struct Fixture {
    data: PathBuf,
    play: FakePlay,
}

/// An F-Droid repository with Notes, served by the fake Play, which offers Chat and Notes too. Play
/// is not enabled.
fn fixture(name: &str) -> Fixture {
    let dir = scratch(name);
    let key = Key::test();
    let repo = dir.join("repo");
    fdroid::write_repo(
        &repo,
        FIRST,
        &[
            fdroid::App::new(NOTES, "Notes", "Write markdown", UPDATED).version(Version::new(
                NOTES,
                2,
                "2.0",
                apk(NOTES, 2, "2.0", &key),
                &key,
            )),
        ],
        &[],
        &key,
    );
    let play = FakePlay::start(Some(&repo));
    play.add(chat(5, "5.0", &key));
    play.add(PlayApp::new(
        NOTES,
        "Notes",
        2,
        "2.0",
        apk(NOTES, 2, "2.0", &key),
        &key.fingerprint(),
    ));
    let data = dir.join("data");
    ok(&uxdroid(&data, &["sources", "disable", "fdroid"]));
    ok(&uxdroid(
        &data,
        &[
            "sources",
            "add",
            "test",
            &play.repo(),
            "--fingerprint",
            &key.fingerprint(),
        ],
    ));
    play.take();
    Fixture { data, play }
}

/// Enables Play and points it at the fake: the API address in the source, the dispenser by option.
fn enable_play(fixture: &Fixture) -> Output {
    let enabled = uxdroid(&fixture.data, &["sources", "enable", "play"]);
    let path = fixture.data.join("store/sources.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for source in config["sources"].as_array_mut().unwrap() {
        if source["kind"] == "play" {
            source["url"] = fixture.play.api().into();
        }
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    ok(&uxdroid(
        &fixture.data,
        &["sources", "play", "--dispenser", &fixture.play.dispenser()],
    ));
    fixture.play.take();
    enabled
}

fn installed(data: &Path, package: &str) -> Option<serde_json::Value> {
    std::fs::read(data.join(format!("app/.{package}.source")))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

/// Every file under `directory`, with its contents, to show a refused command changed nothing.
fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if !path.ends_with(".lock") && !path.to_string_lossy().contains("/store/play/") {
                files.push((path.clone(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn play_is_off_until_enabled_which_warns_once() {
    let fixture = fixture("off");
    let data = &fixture.data;
    let listed = ok(&uxdroid(data, &["sources"]));
    assert!(!listed.contains("play"), "{listed}");
    assert_eq!(
        refused(&uxdroid(data, &["install", CHAT, "--from", "play"])),
        "error: there is no source named play: uxdroid sources lists them\n"
    );
    refused(&uxdroid(data, &["search", "chat"]));
    assert!(
        fixture
            .play
            .take()
            .iter()
            .all(|request| request.path.starts_with("/repo/"))
    );

    let enabled = enable_play(&fixture);
    assert_eq!(enabled.status.code(), Some(0));
    let stdout = text(&enabled.stdout);
    assert!(stdout.starts_with("enabled play\n"), "{stdout}");
    assert!(stdout.contains("\nplay    play    yes"), "{stdout}");
    let warning = text(&enabled.stderr);
    assert!(
        warning.starts_with("warning: Google Play's terms allow only Google's own store app, so Google may limit or block the accounts"),
        "{warning}"
    );
    assert!(warning.contains("third-party service"), "{warning}");
    assert!(fixture.play.take().is_empty(), "enabling asks nothing");

    ok(&uxdroid(data, &["sources", "disable", "play"]));
    let again = uxdroid(data, &["sources", "enable", "play"]);
    assert_eq!(text(&again.stderr), "", "the warning comes once");
    let listed = ok(&uxdroid(data, &["sources"]));
    assert!(
        listed.ends_with(&format!(
            "play signs in anonymously through the token dispenser {}\n",
            fixture.play.dispenser()
        )),
        "{listed}"
    );
}

#[test]
fn search_merges_the_sources_and_names_each() {
    let fixture = fixture("search");
    enable_play(&fixture);
    let out = ok(&uxdroid(&fixture.data, &["search", "chat"]));
    assert_eq!(
        out,
        "NAME            PACKAGE           SOURCE  VERSION  UPDATED\n\
         Chat Messenger  com.example.chat  play    5.0      2025-03-10\n"
    );
    let out = ok(&uxdroid(&fixture.data, &["search", "notes"]));
    assert_eq!(
        out,
        "NAME   PACKAGE            SOURCE  VERSION  UPDATED\n\
         Notes  com.example.notes  test    2.0      2026-10-29\n\
         Notes  com.example.notes  play    2.0      2025-03-10\n"
    );
    // The device described to the dispenser is this machine as the installer sees it.
    let state = fixture.play.state();
    let profile = &state.profiles[0];
    assert_eq!(profile["Platforms"], "x86_64");
    assert_eq!(profile["Build.VERSION.SDK_INT"], "36");
    assert_eq!(profile["Screen.Density"], "160");
    assert_eq!(profile["Locales"], "en");
    let search = state
        .requests
        .iter()
        .rfind(|request| request.path.starts_with("/fdfe/search"))
        .unwrap();
    assert_eq!(search.path, "/fdfe/search?c=3&q=notes&ksm=1");
    assert_eq!(search.header("Authorization"), Some("Bearer fake-auth-1"));
    assert_eq!(search.header("X-DFE-Device-Id"), Some("3787d285572b6434"));
    assert_eq!(state.dispensed, 1, "the sign-in is kept between commands");
}

#[test]
fn install_without_from_prefers_the_repository_and_says_so() {
    let fixture = fixture("prefer");
    enable_play(&fixture);
    let out = uxdroid(&fixture.data, &["install", NOTES]);
    let stdout = ok(&out);
    assert!(
        stdout.starts_with("downloaded com.example.notes 2.0 (2) from test:"),
        "{stdout}"
    );
    assert!(
        text(&out.stderr).contains(
            "note: com.example.notes is in test, so it is installed from there and Google Play is not asked; uxdroid install com.example.notes --from play installs it from Google Play\n"
        ),
        "{}",
        text(&out.stderr)
    );
    assert!(
        fixture
            .play
            .take()
            .iter()
            .all(|request| !request.path.starts_with("/fdfe"))
    );
    assert_eq!(installed(&fixture.data, NOTES).unwrap()["source"], "test");
}

#[test]
fn an_app_only_on_play_installs_from_it_with_its_splits_updates_and_goes() {
    let fixture = fixture("install");
    let data = &fixture.data;
    enable_play(&fixture);
    let out = uxdroid(data, &["install", CHAT]);
    let stdout = ok(&out);
    let size: usize = fixture.play.state().apps[0]
        .files
        .iter()
        .map(|(_, apk)| apk.len())
        .sum();
    assert!(
        stdout.starts_with(&format!(
            "downloaded com.example.chat 5.0 (5) from play: 3 APKs (base, config.x86_64, config.mdpi), {size} bytes\ninstalled com.example.chat: 1 native libraries for x86_64"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("config.mdpi (density), config.x86_64 (abi)"),
        "{stdout}"
    );
    assert_eq!(
        installed(data, CHAT).unwrap(),
        serde_json::json!({"source": "play", "version_code": 5, "version_name": "5.0"})
    );
    let requests = fixture.play.take();
    let paths: Vec<&str> = requests
        .iter()
        .map(|request| request.path.split('?').next().unwrap())
        .filter(|path| !path.starts_with("/repo/"))
        .collect();
    assert_eq!(
        paths,
        [
            "/api/auth",
            "/fdfe/details",
            "/fdfe/purchase",
            "/fdfe/delivery",
            "/download/com.example.chat/base",
            "/download/com.example.chat/config.x86_64",
            "/download/com.example.chat/config.mdpi"
        ]
    );
    assert!(listing(&data.join("store/downloads")).is_empty());

    let list = ok(&uxdroid(data, &["list"]));
    assert!(list.contains("com.example.chat  play    5.0 (5)"), "{list}");
    assert_eq!(
        ok(&uxdroid(data, &["update"])),
        "com.example.chat 5.0 (5) from play is up to date\n"
    );
    assert_eq!(
        ok(&uxdroid(data, &["install", CHAT])),
        "com.example.chat 5.0 (5) from play is already installed\n"
    );

    fixture.play.add(chat(6, "6.0", &Key::test()));
    let out = ok(&uxdroid(data, &["update", CHAT]));
    assert!(
        out.ends_with("updated com.example.chat from 5.0 (5) to 6.0 (6) from play\n"),
        "{out}"
    );
    assert_eq!(installed(data, CHAT).unwrap()["version_code"], 6);

    assert_eq!(
        ok(&uxdroid(data, &["remove", CHAT])),
        "removed com.example.chat: its code, data, signer record and component settings\n"
    );
    assert!(installed(data, CHAT).is_none());
}

#[test]
fn the_certificate_play_names_does_not_stop_an_app_that_rotated_its_key() {
    // Play names an app's original certificate, which for a rotated key is not the APK's signer.
    let fixture = fixture("rotated");
    enable_play(&fixture);
    let mut app = chat(5, "5.0", &Key::test());
    app.signer = Key::other().fingerprint();
    fixture.play.add(app);
    ok(&uxdroid(
        &fixture.data,
        &["install", CHAT, "--from", "play"],
    ));
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("app/.com.example.chat.signer")).unwrap(),
        format!("{}\n", Key::test().fingerprint())
    );
}

#[test]
fn an_update_from_play_signed_by_another_key_is_refused_and_changes_nothing() {
    let fixture = fixture("signer");
    let data = &fixture.data;
    enable_play(&fixture);
    ok(&uxdroid(data, &["install", CHAT]));
    let before = snapshot(data);
    fixture.play.add(chat(6, "6.0", &Key::other()));
    let error = text(&uxdroid(data, &["update", CHAT]).stderr);
    assert!(
        error.contains("error: cannot update com.example.chat: com.example.chat is installed signed by a different certificate, so this update is refused"),
        "{error}"
    );
    assert_eq!(snapshot(data), before);
}

/// A change to an app on the fake, given the fake.
type Change = Box<dyn Fn(&mut PlayApp, &FakePlay)>;

#[test]
fn files_play_delivers_must_match_what_it_states_and_come_from_it() {
    let key = Key::test();
    let cases: Vec<(&str, Change, &str)> = vec![
        (
            "sha",
            Box::new(|app, _| {
                app.stated_sha256.insert("config.mdpi".into(), vec![7; 32]);
            }),
            "the split config.mdpi of com.example.chat from play does not match its index (SHA-256",
        ),
        (
            "bigger",
            Box::new(|app, _| {
                let size = app.files[0].1.len() as u64 - 10;
                app.stated_size.insert("base".into(), size);
            }),
            "the APK of com.example.chat from play is larger than the",
        ),
        (
            "host",
            Box::new(|app, _| {
                app.urls
                    .insert("config.x86_64".into(), "https://example.org/x.apk".into());
            }),
            "Google Play's delivery of com.example.chat gives config.x86_64 the address https://example.org/x.apk, which is not Google Play's, so it is refused",
        ),
        (
            "scheme",
            Box::new(|app, _| {
                app.urls.insert("base".into(), "file:///etc/passwd".into());
            }),
            "gives base the address file:///etc/passwd, which is not Google Play's, so it is refused",
        ),
        (
            "port",
            Box::new(|app, play| {
                let other = play.origin().replace("127.0.0.1", "127.0.0.2");
                app.urls
                    .insert("base".into(), format!("{other}/download/x/base"));
            }),
            "which is not Google Play's, so it is refused",
        ),
        (
            "name",
            Box::new(|app, _| {
                app.files[2].0 = "../../evil".into();
            }),
            "Google Play's delivery of com.example.chat names a split ../../evil, which is not a valid or a new split name, so it is refused",
        ),
        (
            "unavailable",
            Box::new(|app, _| app.delivery_status = 2),
            "Google Play does not deliver com.example.chat 5 to this device (x86_64, SDK 36) (status 2)",
        ),
        (
            "paid",
            Box::new(|app, _| {
                app.price_micros = 1_990_000;
                app.price_shown = "$1.99".into();
            }),
            "com.example.chat costs $1.99 on Google Play, and uxdroid installs only free apps from it",
        ),
    ];
    for (name, change, expected) in cases {
        let fixture = fixture(&format!("hostile-{name}"));
        enable_play(&fixture);
        let mut app = chat(5, "5.0", &key);
        change(&mut app, &fixture.play);
        fixture.play.add(app);
        let before = snapshot(&fixture.data);
        let error = refused(&uxdroid(
            &fixture.data,
            &["install", CHAT, "--from", "play"],
        ));
        assert!(error.contains(expected), "{name}: {error}");
        assert!(installed(&fixture.data, CHAT).is_none(), "{name}");
        assert!(
            !fixture.data.join("app/com.example.chat").exists(),
            "{name}"
        );
        assert!(
            listing(&fixture.data.join("store/downloads")).is_empty(),
            "{name}"
        );
        assert_eq!(snapshot(&fixture.data).len(), before.len(), "{name}");
        let downloads = fixture
            .play
            .take()
            .into_iter()
            .filter(|request| request.path.starts_with("/download/") && name == "host")
            .count();
        assert_eq!(downloads, 0, "nothing is fetched from another host");
    }
}

#[test]
fn malformed_and_oversized_answers_from_play_are_refused() {
    let truncated = Proto::new()
        .message(1, Proto::new().message(2, Proto::new().string(4, "cut")))
        .0;
    let cases: Vec<(&str, Answer, &str)> = vec![
        (
            "truncated",
            Answer::Raw(truncated[..truncated.len() - 2].to_vec()),
            "Google Play answered with a malformed message (a field is longer than what is left of the message), so it is refused",
        ),
        (
            "huge-length",
            Answer::Raw(vec![
                0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
            ]),
            "Google Play answered with a malformed message",
        ),
        (
            "group",
            Answer::Raw(vec![0x0b, 0x0c]),
            "a field has an unknown or group wire type",
        ),
        (
            "oversized",
            Answer::Zeros(9 * 1024 * 1024),
            "bytes allowed, so it is refused",
        ),
        (
            "error",
            Answer::Status(500),
            "Google Play answered 500 to the details of com.example.chat",
        ),
    ];
    for (name, answer, expected) in cases {
        let fixture = fixture(&format!("answer-{name}"));
        enable_play(&fixture);
        fixture.play.answer("/fdfe/details", answer);
        let error = refused(&uxdroid(
            &fixture.data,
            &["install", CHAT, "--from", "play"],
        ));
        assert!(error.contains(expected), "{name}: {error}");
        assert!(installed(&fixture.data, CHAT).is_none(), "{name}");
    }
    // A malformed dispenser answer is not kept as a sign-in.
    let fixture = fixture("answer-dispenser");
    enable_play(&fixture);
    fixture.play.answer(
        "/api/auth",
        Answer::Raw(br#"{"authToken": "x\r\nInjected: 1", "gsfId": "12"}"#.to_vec()),
    );
    let error = refused(&uxdroid(&fixture.data, &["search", "chat"]));
    assert!(
        error.contains("gave a malformed sign-in, so it is not used"),
        "{error}"
    );
    assert!(!fixture.data.join("store/play/session.json").exists());
}

#[test]
fn an_expired_sign_in_is_renewed_once() {
    let fixture = fixture("expired");
    enable_play(&fixture);
    ok(&uxdroid(&fixture.data, &["search", "chat"]));
    fixture.play.expire();
    fixture.play.take();
    ok(&uxdroid(&fixture.data, &["search", "chat"]));
    let paths: Vec<String> = fixture
        .play
        .take()
        .into_iter()
        .map(|request| request.path.split('?').next().unwrap().to_string())
        .filter(|path| !path.starts_with("/repo/"))
        .collect();
    assert_eq!(paths, ["/fdfe/search", "/api/auth", "/fdfe/search"]);

    fixture.play.answer("/api/auth", Answer::Status(503));
    fixture.play.expire();
    let error = refused(&uxdroid(
        &fixture.data,
        &["install", CHAT, "--from", "play"],
    ));
    assert!(
        error.contains(&format!(
            "the token dispenser {} answered 503, so Google Play cannot be signed in to anonymously now",
            fixture.play.dispenser()
        )),
        "{error}"
    );
}

#[test]
fn an_account_signs_in_with_its_own_token_which_stays_private() {
    let fixture = fixture("account");
    let data = &fixture.data;
    enable_play(&fixture);
    let out = uxdroid_with(
        data,
        &["sources", "play", "--account", "me@example.org"],
        &format!("{OAUTH_TOKEN}\n"),
    );
    let stdout = ok(&out);
    assert_eq!(
        stdout,
        "play signs in as me@example.org\nits token is kept readable only by you, and is never printed\n"
    );
    let all = format!("{stdout}{}", text(&out.stderr));
    assert!(
        !all.contains("fake-oauth") && !all.contains("fake-account"),
        "{all}"
    );
    let token = data.join("store/play/token");
    assert_eq!(
        std::fs::read_to_string(&token).unwrap(),
        AAS_TOKEN,
        "only the AAS token is kept"
    );
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(data.join("store/play"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let out = uxdroid(data, &["install", CHAT, "--from", "play"]);
    ok(&out);
    assert!(!text(&out.stdout).contains("fake-auth") && !text(&out.stderr).contains("fake-auth"));
    let paths: Vec<String> = fixture
        .play
        .take()
        .into_iter()
        .map(|request| request.path.split('?').next().unwrap().to_string())
        .filter(|path| !path.starts_with("/repo/") && !path.starts_with("/download/"))
        .collect();
    assert_eq!(
        paths,
        [
            "/auth",
            "/checkin",
            "/auth",
            "/fdfe/uploadDeviceConfig",
            "/fdfe/details",
            "/fdfe/purchase",
            "/fdfe/delivery"
        ]
    );
    let session = std::fs::metadata(data.join("store/play/session.json")).unwrap();
    assert_eq!(session.permissions().mode() & 0o777, 0o600);
    assert_eq!(installed(data, CHAT).unwrap()["source"], "play");

    assert_eq!(
        ok(&uxdroid(data, &["sources", "play", "--anonymous"])),
        format!(
            "play signs in anonymously through the token dispenser {}\n",
            fixture.play.dispenser()
        )
    );
    assert!(
        !token.exists(),
        "going anonymous forgets the account's token"
    );

    let bad = refused(&uxdroid_with(
        data,
        &["sources", "play", "--account", "me@example.org"],
        "hunter2\n",
    ));
    assert!(bad.contains("is neither an AAS token"), "{bad}");
    assert!(!bad.contains("hunter2"), "{bad}");
    let none = refused(&uxdroid(
        data,
        &["sources", "play", "--account", "me@example.org"],
    ));
    assert!(
        none.contains("no token was given for me@example.org"),
        "{none}"
    );
}

#[test]
fn sources_play_options_are_checked() {
    let fixture = fixture("options");
    let data = &fixture.data;
    for args in [
        &[
            "sources",
            "play",
            "--anonymous",
            "--account",
            "me@example.org",
        ][..],
        &["sources", "play", "extra"],
        &["sources", "--anonymous"],
        &["sources", "enable", "play", "--anonymous"],
        &["sources", "play", "--fingerprint", "00"],
    ] {
        let out = uxdroid(data, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("uxdroid sources play [--anonymous | --account <email>] [--dispenser <url>] [--data <dir>]"));
    }
    let out = ok(&uxdroid(data, &["sources", "play"]));
    assert_eq!(
        out,
        "play signs in anonymously through the token dispenser https://auroraoss.com/api/auth\nplay is not enabled: uxdroid sources enable play enables it\n"
    );
    let error = refused(&uxdroid(
        data,
        &["sources", "play", "--dispenser", "http://example.org/auth"],
    ));
    assert!(error.contains("is not a repository address"), "{error}");
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
