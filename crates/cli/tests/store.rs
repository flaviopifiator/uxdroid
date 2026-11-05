//! `uxdroid sources`, `search`, `install <package>`, `update`, `remove` and `list` against F-Droid
//! repositories made by the testkit and served by a local HTTP server, so no test needs the network.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use uxdroid_testkit::fdroid::{self, App, Version, sha256_hex};
use uxdroid_testkit::{Apk, Key, app_manifest, dex};

const UPDATED: u64 = 1_793_273_970_519; // 2026-10-29
const OLDER: u64 = 1_791_000_000_000; // 2026-10-03
const FIRST: u64 = 1_793_000_000_000;
const SECOND: u64 = 1_793_100_000_000;

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("store-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Serves `root` under `/repo/` over HTTP/1.1, one connection a request, and records each path.
/// Paths can be made to stall after their headers, or to redirect.
struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    behavior: Arc<Mutex<Vec<(String, Behavior)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum Behavior {
    /// Promises a body and never sends it.
    Stall,
    /// Answers 302 with this location.
    Redirect(String),
}

impl Server {
    fn start(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let behavior = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (root, requests, behavior, stop) = (
                root.to_path_buf(),
                requests.clone(),
                behavior.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve(stream, &root, &requests, &behavior, &stop);
                    }
                }
            })
        };
        Self {
            port,
            requests,
            behavior,
            stop,
            thread: Some(thread),
        }
    }

    fn behave(&self, path: &str, behavior: Behavior) {
        self.behavior
            .lock()
            .unwrap()
            .push((path.to_string(), behavior));
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/repo", self.port)
    }

    /// The paths asked for since the last call.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    root: &Path,
    requests: &Mutex<Vec<String>>,
    behavior: &Mutex<Vec<(String, Behavior)>>,
    stop: &AtomicBool,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
    }
    let path = line.split(' ').nth(1).unwrap_or("").to_string();
    requests.lock().unwrap().push(path.clone());
    let special = behavior
        .lock()
        .unwrap()
        .iter()
        .find(|(special, _)| *special == path)
        .map(|(_, behavior)| behavior.clone());
    let mut stream = stream;
    match special {
        Some(Behavior::Stall) => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nPK",
            );
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            return;
        }
        Some(Behavior::Redirect(location)) => {
            let _ = write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            return;
        }
        None => {}
    }
    let file = path
        .strip_prefix("/repo/")
        .filter(|name| !name.contains(".."))
        .map(|name| root.join(name));
    match file.and_then(|file| std::fs::read(file).ok()) {
        Some(body) => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
        None => {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
}

/// Runs uxdroid on `data` with no framework runtime and no proxy, as installs in tests do.
fn uxdroid(data: &Path, args: &[&str]) -> Output {
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
    command.output().unwrap()
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

fn apk(package: &'static str, code: u32, name: &'static str, key: &Key) -> Vec<u8> {
    Apk::new()
        .manifest(&app_manifest(package, code, name))
        .file("classes.dex", &dex(1, 1))
        .signed_bytes(key)
}

/// Notes has a beta, a version only for ARM, a version for a future SDK, and the one that fits;
/// Markdown Viewer mentions notes in its summary; Future Notes fits nothing here.
fn catalog() -> Vec<App> {
    let key = Key::test();
    let notes = "com.example.notes";
    let mut beta = Version::new(notes, 5, "5.0-beta", apk(notes, 5, "5.0-beta", &key), &key);
    beta.release_channels = vec!["Beta".into()];
    let mut arm = Version::new(notes, 4, "4.0", apk(notes, 4, "4.0", &key), &key);
    arm.nativecode = vec!["arm64-v8a".into(), "armeabi-v7a".into()];
    let mut future = Version::new(notes, 3, "3.0", apk(notes, 3, "3.0", &key), &key);
    future.min_sdk = 40;
    let viewer = "com.example.markdown";
    let mut only_future = Version::new(
        "com.example.future",
        1,
        "1.0",
        apk("com.example.future", 1, "1.0", &key),
        &key,
    );
    only_future.max_sdk = Some(30);
    vec![
        App::new(
            viewer,
            "Markdown Viewer",
            "Reads notes and documents",
            OLDER,
        )
        .version(Version::new(
            viewer,
            1,
            "1.0",
            apk(viewer, 1, "1.0", &key),
            &key,
        )),
        App::new(notes, "Notes", "Write markdown", UPDATED)
            .version(beta)
            .version(arm)
            .version(future)
            .version(Version::new(
                notes,
                2,
                "2.0",
                apk(notes, 2, "2.0", &key),
                &key,
            ))
            .version(Version::new(
                notes,
                1,
                "1.0",
                apk(notes, 1, "1.0", &key),
                &key,
            )),
        App::new("com.example.future", "Future Notes", "Not yet", UPDATED).version(only_future),
    ]
}

/// A repository of `apps` signed by the test key, served, and the only enabled source of `data`.
struct Fixture {
    server: Server,
    repo: PathBuf,
    data: PathBuf,
    index: Vec<u8>,
}

fn repository(dir: &Path, apps: &[App]) -> Fixture {
    let repo = dir.join("repo");
    let index = fdroid::write_repo(&repo, FIRST, apps, &[], &Key::test());
    let server = Server::start(&repo);
    let data = dir.join("data");
    ok(&uxdroid(&data, &["sources", "disable", "fdroid"]));
    ok(&uxdroid(
        &data,
        &[
            "sources",
            "add",
            "test",
            &server.url(),
            "--fingerprint",
            &Key::test().fingerprint(),
        ],
    ));
    server.take();
    Fixture {
        server,
        repo,
        data,
        index,
    }
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

const INSTALL_USAGE: &str = "usage: uxdroid install <apk>... --data <dir>\n       uxdroid install <package> [--from <source>] [--refresh] [--data <dir>]\n";

#[test]
fn store_commands_print_their_usage_and_exit_2() {
    let data = scratch("usage").join("data");
    let search = "usage: uxdroid search <text>... [--refresh] [--limit <n>] [--data <dir>]\n";
    for (args, usage) in [
        (&["search"][..], search),
        (&["search", "x", "--limit", "0"], search),
        (&["search", "x", "--refresh", "--refresh"], search),
        (
            &["update", "a", "b"],
            "usage: uxdroid update [<package>] [--refresh] [--data <dir>]\n",
        ),
        (
            &["remove"],
            "usage: uxdroid remove <package> [--data <dir>]\n",
        ),
        (&["list", "x"], "usage: uxdroid list [--data <dir>]\n"),
        (&["install", "com.example.app", "--other"], INSTALL_USAGE),
        (&["install", "a.apk", "--from", "test"], INSTALL_USAGE),
    ] {
        let output = uxdroid(&data, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&output.stderr), usage, "{args:?}");
    }
    for args in [
        &["sources", "add", "x", "http://h"][..],
        &["sources", "remove", "x"],
    ] {
        let output = uxdroid(&data, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(text(&output.stderr).starts_with("usage: uxdroid sources [--data <dir>]\n"));
    }
    assert!(!data.exists());
}

#[test]
fn the_official_fdroid_repository_is_a_source_by_default() {
    let data = scratch("default").join("data");

    let output = uxdroid(&data, &["sources"]);

    assert_eq!(
        ok(&output),
        "NAME    KIND    ENABLED  URL                       FINGERPRINT\n\
         fdroid  fdroid  yes      https://f-droid.org/repo  43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab\n"
    );
    // Listing reads nothing from the network and writes nothing.
    assert!(!data.exists());
}

#[test]
fn a_third_party_repository_is_added_by_url_and_fingerprint_and_can_be_disabled_and_enabled() {
    let data = scratch("add").join("data");
    let fingerprint = "AB:".repeat(31) + "AB";

    let added = uxdroid(
        &data,
        &[
            "sources",
            "add",
            "izzy",
            "https://example.org/fdroid/repo/",
            "--fingerprint",
            &fingerprint,
        ],
    );
    let disabled = uxdroid(&data, &["sources", "disable", "fdroid"]);

    assert_eq!(
        ok(&added),
        format!(
            "added izzy\n\
             NAME    KIND    ENABLED  URL                              FINGERPRINT\n\
             fdroid  fdroid  yes      https://f-droid.org/repo         43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab\n\
             izzy    fdroid  yes      https://example.org/fdroid/repo  {}\n",
            "ab".repeat(32)
        )
    );
    assert!(ok(&disabled).contains("\nfdroid  fdroid  no       https://f-droid.org/repo"));
    let enabled = ok(&uxdroid(&data, &["sources", "enable", "fdroid"]));
    assert!(enabled.starts_with("enabled fdroid\n"));
    assert!(enabled.contains("\nfdroid  fdroid  yes      https://f-droid.org/repo"));
    let config = std::fs::read(data.join("store/sources.json")).unwrap();

    let zeros = "0".repeat(64);
    for (args, error) in [
        (
            &[
                "sources",
                "add",
                "izzy",
                "https://other.org/repo",
                "--fingerprint",
                &zeros,
            ][..],
            "error: there is already a source named izzy\n",
        ),
        (
            &[
                "sources",
                "add",
                "bad",
                "https://other.org/repo",
                "--fingerprint",
                "1234",
            ],
            "error: 1234 is not a SHA-256 fingerprint: give the 64 hex digits of the repository's signing certificate\n",
        ),
        (
            &[
                "sources",
                "add",
                "bad",
                "ftp://other.org/repo",
                "--fingerprint",
                &zeros,
            ],
            "error: ftp://other.org/repo is not a repository address: give an https URL without a query (plain http only for this machine, such as http://127.0.0.1)\n",
        ),
        (
            &[
                "sources",
                "add",
                "../x",
                "https://other.org/repo",
                "--fingerprint",
                &zeros,
            ],
            "error: ../x is not a source name: use at most 32 lowercase letters, digits and -\n",
        ),
        (
            &["sources", "enable", "nope"],
            "error: there is no source named nope: uxdroid sources lists them\n",
        ),
    ] {
        let output = uxdroid(&data, args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(text(&output.stderr), error, "{args:?}");
        assert_eq!(
            std::fs::read(data.join("store/sources.json")).unwrap(),
            config
        );
    }
}

#[test]
fn search_lists_name_package_source_version_and_last_update_best_match_first() {
    let dir = scratch("search");
    let fixture = repository(&dir, &catalog());

    let output = uxdroid(&fixture.data, &["search", "notes"]);

    assert_eq!(
        ok(&output),
        "NAME             PACKAGE               SOURCE  VERSION  UPDATED\n\
         Notes            com.example.notes     test    2.0      2026-10-29\n\
         Markdown Viewer  com.example.markdown  test    1.0      2026-10-03\n"
    );
    assert_eq!(
        text(&output.stderr),
        format!(
            "note: fetching the index of test ({} bytes)\n\
             note: 1 more app matches but has no version for x86_64 and SDK 36\n",
            fixture.index.len()
        )
    );
    assert_eq!(
        fixture.server.take(),
        ["/repo/entry.jar", "/repo/index-v2.json"]
    );
    // More words narrow it, the limit cuts it, and nothing found fails.
    let both = uxdroid(
        &fixture.data,
        &["search", "markdown", "notes", "--limit", "1"],
    );
    assert_eq!(
        ok(&both),
        "NAME   PACKAGE            SOURCE  VERSION  UPDATED\n\
         Notes  com.example.notes  test    2.0      2026-10-29\n"
    );
    assert_eq!(
        text(&both.stderr),
        "note: 1 more app matches; --limit <n> shows more\n"
    );
    let none = uxdroid(&fixture.data, &["search", "spreadsheet"]);
    assert_eq!(none.status.code(), Some(1));
    assert_eq!(text(&none.stderr), "error: no app matches spreadsheet\n");
}

#[test]
fn search_reads_the_cached_index_without_the_network_and_notes_when_it_is_stale() {
    let dir = scratch("offline");
    let fixture = repository(&dir, &catalog());
    let first = ok(&uxdroid(&fixture.data, &["search", "viewer"]));
    let data = fixture.data.clone();
    drop(fixture.server);

    let fresh = uxdroid(&data, &["search", "viewer"]);
    ok(&uxdroid(&data, &["sources", "refresh-after", "0"]));
    let stale = uxdroid(&data, &["search", "viewer"]);

    assert_eq!(ok(&fresh), first);
    assert_eq!(text(&fresh.stderr), "");
    assert_eq!(ok(&stale), first);
    let note = text(&stale.stderr);
    assert!(
        note.starts_with(
            "note: the index of test was last checked 0 hours ago and could not be refreshed: cannot fetch http://127.0.0.1:"
        ),
        "{note}"
    );
}

#[test]
fn an_index_is_fetched_again_when_older_than_the_interval_or_with_refresh() {
    let dir = scratch("refresh");
    let fixture = repository(&dir, &catalog());
    ok(&uxdroid(&fixture.data, &["search", "notes"]));
    fixture.server.take();

    ok(&uxdroid(&fixture.data, &["search", "notes"]));
    let warm = fixture.server.take();
    ok(&uxdroid(&fixture.data, &["search", "notes", "--refresh"]));
    let forced = fixture.server.take();
    ok(&uxdroid(&fixture.data, &["sources", "refresh-after", "0"]));
    ok(&uxdroid(&fixture.data, &["search", "notes"]));
    let stale = fixture.server.take();

    assert!(warm.is_empty(), "{warm:?}");
    // The entry has not changed, so the index is not read again.
    assert_eq!(forced, ["/repo/entry.jar"]);
    assert_eq!(stale, ["/repo/entry.jar"]);
}

#[test]
fn a_diff_from_the_cached_index_updates_it() {
    let dir = scratch("diff");
    let key = Key::test();
    let old = catalog();
    let fixture = repository(&dir, &old);
    ok(&uxdroid(&fixture.data, &["search", "notes"]));
    let mut new = old.clone();
    let notes = "com.example.notes";
    new[1].versions.push(Version::new(
        notes,
        6,
        "6.0",
        apk(notes, 6, "6.0", &key),
        &key,
    ));
    new[1].last_updated = UPDATED + 86_400_000;
    new.remove(0);
    let patch = fdroid::diff(&fdroid::index(FIRST, &old), &fdroid::index(SECOND, &new));
    fdroid::write_repo(
        &fixture.repo,
        SECOND,
        &new,
        &[(FIRST, serde_json::to_vec(&patch).unwrap())],
        &key,
    );
    fixture.server.take();

    let output = uxdroid(&fixture.data, &["search", "notes", "--refresh"]);

    assert_eq!(
        ok(&output),
        "NAME   PACKAGE            SOURCE  VERSION  UPDATED\n\
         Notes  com.example.notes  test    6.0      2026-10-30\n"
    );
    assert_eq!(
        fixture.server.take(),
        ["/repo/entry.jar", &format!("/repo/diff/{FIRST}.json")]
    );
}

/// Runs a search that must be refused with `error`, leaving no index cached.
fn assert_refused(fixture: &Fixture, error: &str) {
    let output = uxdroid(&fixture.data, &["search", "notes"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout));
    let stderr = text(&output.stderr);
    assert!(stderr.contains(error), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!fixture.data.join("store/sources/test/index.json").exists());
}

#[test]
fn a_tampered_entry_jar_is_refused() {
    let dir = scratch("tampered-entry");
    let fixture = repository(&dir, &catalog());
    let index = std::fs::read(fixture.repo.join("index-v2.json")).unwrap();
    let mut lying = fdroid::entry(FIRST, &index, &[]);
    lying.extend_from_slice(b" ");
    let signed = Apk::new()
        .file("entry.json", &fdroid::entry(FIRST, &index, &[]))
        .with_v1_signature(&Key::test(), None)
        .replace("entry.json", &lying)
        .bytes();
    std::fs::write(fixture.repo.join("entry.jar"), signed).unwrap();

    assert_refused(
        &fixture,
        &format!(
            "error: the index of test ({}) is refused: entry.jar does not verify: its signature does not verify: entry.json does not match its signed digest, so it was changed after signing\n",
            fixture.server.url()
        ),
    );
}

#[test]
fn an_entry_jar_signed_by_a_key_other_than_the_pinned_one_is_refused() {
    let dir = scratch("wrong-key");
    let fixture = repository(&dir, &catalog());
    let index = std::fs::read(fixture.repo.join("index-v2.json")).unwrap();
    std::fs::write(
        fixture.repo.join("entry.jar"),
        fdroid::entry_jar(&fdroid::entry(FIRST, &index, &[]), &Key::other()),
    )
    .unwrap();

    assert_refused(
        &fixture,
        &format!(
            "is refused: entry.jar is signed by {}, not by the certificate pinned for the source, {}\n",
            Key::other().fingerprint(),
            Key::test().fingerprint()
        ),
    );
}

#[test]
fn an_index_that_does_not_match_its_signed_hash_is_refused() {
    let dir = scratch("tampered-index");
    let fixture = repository(&dir, &catalog());
    let mut index = std::fs::read(fixture.repo.join("index-v2.json")).unwrap();
    let at = index.len() - 2;
    index[at] = b' ';
    std::fs::write(fixture.repo.join("index-v2.json"), &index).unwrap();

    assert_refused(
        &fixture,
        "is refused: its index does not have the SHA-256 its signed entry gives, so it was changed after signing\n",
    );
}

#[test]
fn a_signed_entry_older_than_the_cached_index_is_refused_as_a_replay() {
    let dir = scratch("replay");
    let fixture = repository(&dir, &catalog());
    let old_jar = std::fs::read(fixture.repo.join("entry.jar")).unwrap();
    fdroid::write_repo(&fixture.repo, SECOND, &catalog()[..2], &[], &Key::test());
    let current = ok(&uxdroid(&fixture.data, &["search", "notes"]));
    std::fs::write(fixture.repo.join("entry.jar"), old_jar).unwrap();

    let output = uxdroid(&fixture.data, &["search", "notes", "--refresh"]);

    assert_eq!(ok(&output), current);
    assert!(
        text(&output.stderr).contains(&format!(
            "could not be refreshed: the index of test ({}) is refused: its signed entry is from {FIRST}, older than the index already read ({SECOND}), so it may be a replay\n",
            fixture.server.url()
        )),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn oversized_downloads_are_refused() {
    let dir = scratch("oversized");
    let key = Key::test();
    let mut apps = catalog();
    let mut big = Version::new(
        "com.example.big",
        1,
        "1.0",
        apk("com.example.big", 1, "1.0", &key),
        &key,
    );
    big.size = Some(100);
    apps.push(App::new("com.example.big", "Big", "Larger than it says", UPDATED).version(big));
    let fixture = repository(&dir, &apps);

    let install = uxdroid(&fixture.data, &["install", "com.example.big"]);
    // An index larger than its signed size is not read past that size.
    let mut index = std::fs::read(fixture.repo.join("index-v2.json")).unwrap();
    index.extend(std::iter::repeat_n(b' ', 1000));
    std::fs::write(fixture.repo.join("index-v2.json"), &index).unwrap();
    std::fs::remove_dir_all(fixture.data.join("store/sources")).unwrap();
    let search = uxdroid(&fixture.data, &["search", "notes"]);

    assert_eq!(install.status.code(), Some(1));
    assert!(
        text(&install.stderr).ends_with(
            "error: the APK of com.example.big from test is larger than the 100 bytes its index gives, so it is refused\n"
        ),
        "{}",
        text(&install.stderr)
    );
    assert_eq!(listing(&fixture.data.join("app")), Vec::<String>::new());
    assert_eq!(
        listing(&fixture.data.join("store/downloads")),
        Vec::<String>::new()
    );
    assert_eq!(search.status.code(), Some(1));
    assert!(
        text(&search.stderr)
            .contains("is refused: its index is larger than its signed entry says\n"),
        "{}",
        text(&search.stderr)
    );
}

#[test]
fn names_from_the_index_that_could_leave_their_directory_are_never_used() {
    let dir = scratch("traversal");
    let key = Key::test();
    let mut apps = catalog();
    let mut escape = Version::new(
        "com.example.evil",
        1,
        "1.0",
        apk("com.example.evil", 1, "1.0", &key),
        &key,
    );
    escape.file = "/../../evil.apk".into();
    apps.push(App::new("com.example.evil", "Evil notes", "Escapes", UPDATED).version(escape));
    apps.push(
        App::new("../../evil", "Evil package notes", "Escapes", UPDATED).version(Version::new(
            "evil",
            1,
            "1.0",
            apk("com.example.app", 1, "1.0", &key),
            &key,
        )),
    );
    let fixture = repository(&dir, &apps);

    let search = uxdroid(&fixture.data, &["search", "evil"]);
    let install = uxdroid(&fixture.data, &["install", "com.example.evil"]);
    let traversal = uxdroid(&fixture.data, &["install", "../../evil", "--from", "test"]);

    assert_eq!(search.status.code(), Some(1));
    assert!(text(&search.stderr).ends_with("error: no app matches evil\n"));
    assert_eq!(install.status.code(), Some(1));
    assert_eq!(
        text(&install.stderr),
        "note: com.example.evil is not a file, so it is looked up as a package name in the enabled sources\n\
         error: no enabled source has com.example.evil: uxdroid search finds apps\n"
    );
    assert_eq!(traversal.status.code(), Some(1));
    assert_eq!(
        text(&traversal.stderr),
        "error: bad package name ../../evil\n"
    );
    assert!(
        !fixture
            .server
            .take()
            .iter()
            .any(|path| path.contains("evil"))
    );
    assert_eq!(listing(&dir), ["data", "repo"]);
    assert_eq!(listing(&fixture.data.join("app")), Vec::<String>::new());
}

/// The files an install leaves for `package` in `data/app`.
fn app_files(data: &Path, package: &str) -> Vec<String> {
    listing(&data.join("app"))
        .into_iter()
        .filter(|name| name.contains(package))
        .collect()
}

#[test]
fn install_of_a_package_takes_the_newest_version_for_this_machine_and_records_its_source() {
    let dir = scratch("install");
    let fixture = repository(&dir, &catalog());
    let fits = &catalog()[1].versions[3];

    let output = uxdroid(&fixture.data, &["install", "com.example.notes"]);

    assert_eq!(
        ok(&output),
        format!(
            "downloaded com.example.notes 2.0 (2) from test: {} bytes, SHA-256 {}\n\
             installed com.example.notes: 0 native libraries for x86_64 in {}\n",
            fits.apk.len(),
            sha256_hex(&fits.apk),
            fixture.data.join("data/com.example.notes/lib").display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("app/.com.example.notes.source")).unwrap(),
        "{\"source\":\"test\",\"version_code\":2,\"version_name\":\"2.0\"}\n"
    );
    assert_eq!(
        app_files(&fixture.data, "com.example.notes"),
        [
            ".com.example.notes.lock",
            ".com.example.notes.signer",
            ".com.example.notes.source"
        ]
    );
    assert_eq!(
        listing(&fixture.data.join("store/downloads")),
        Vec::<String>::new()
    );
    assert_eq!(
        ok(&uxdroid(&fixture.data, &["install", "com.example.notes"])),
        "com.example.notes 2.0 (2) from test is already installed\n"
    );
    let unfit = uxdroid(&fixture.data, &["install", "com.example.future"]);
    assert_eq!(unfit.status.code(), Some(1));
    assert_eq!(
        text(&unfit.stderr),
        "note: com.example.future is not a file, so it is looked up as a package name in the enabled sources\n\
         error: com.example.future in test has no version for x86_64 and SDK 36: its newest, 1.0 (1), needs SDK 30 or earlier\n"
    );
}

#[test]
fn list_shows_installed_apps_with_source_and_version() {
    let dir = scratch("list");
    let fixture = repository(&dir, &catalog());
    assert_eq!(
        ok(&uxdroid(&fixture.data, &["list"])),
        format!("no app is installed in {}\n", fixture.data.display())
    );
    ok(&uxdroid(&fixture.data, &["install", "com.example.notes"]));
    let file = dir.join("app.apk");
    std::fs::write(&file, apk("com.example.app", 42, "1.2.3", &Key::test())).unwrap();
    ok(&uxdroid(
        &fixture.data,
        &["install", file.to_str().unwrap()],
    ));

    let output = uxdroid(&fixture.data, &["list"]);

    // Without a framework an app from a file keeps no APK to read its version from.
    assert_eq!(
        ok(&output),
        "PACKAGE            SOURCE  VERSION\n\
         com.example.app    file    unknown\n\
         com.example.notes  test    2.0 (2)\n"
    );
}

#[test]
fn an_apk_that_does_not_match_its_index_hash_is_refused_and_nothing_is_installed() {
    let dir = scratch("bad-apk");
    let fixture = repository(&dir, &catalog());
    let file = fixture.repo.join("com.example.notes_2.apk");
    let other = apk("com.example.notes", 2, "2.0", &Key::other());
    let mut same_size = other.clone();
    same_size.resize(std::fs::metadata(&file).unwrap().len() as usize, 0);
    std::fs::write(&file, &same_size).unwrap();

    let output = uxdroid(&fixture.data, &["install", "com.example.notes"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains(&format!(
            "error: the APK of com.example.notes from test does not match its index (SHA-256 {}",
            sha256_hex(&same_size)
        )),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(listing(&fixture.data.join("app")), Vec::<String>::new());
    assert!(!fixture.data.join("data").exists());
    assert_eq!(
        listing(&fixture.data.join("store/downloads")),
        Vec::<String>::new()
    );
}

#[test]
fn an_apk_whose_package_version_or_signer_differs_from_its_index_is_refused() {
    let dir = scratch("mismatch");
    let key = Key::test();
    let mut apps = Vec::new();
    for (package, apk_bytes, signer) in [
        (
            "com.example.one",
            apk("com.example.app", 1, "1.0", &key),
            key.fingerprint(),
        ),
        (
            "com.example.two",
            apk("com.example.two", 9, "1.0", &key),
            key.fingerprint(),
        ),
        (
            "com.example.three",
            apk("com.example.three", 1, "1.0", &Key::other()),
            key.fingerprint(),
        ),
    ] {
        let mut version = Version::new(package, 1, "1.0", apk_bytes, &key);
        version.signer = Some(vec![signer]);
        apps.push(App::new(package, package, "", UPDATED).version(version));
    }
    let fixture = repository(&dir, &apps);

    for (package, error) in [
        ("com.example.one", "error: the APK of com.example.one from test is the package com.example.app, so it is refused\n".to_string()),
        ("com.example.two", "error: the APK of com.example.two from test has version code 9, and its index gives 1, so it is refused\n".to_string()),
        ("com.example.three", format!("error: the APK of com.example.three from test is signed by {}, and its index says {}, so it is refused\n", Key::other().fingerprint(), key.fingerprint())),
    ] {
        let output = uxdroid(&fixture.data, &["install", package]);
        assert_eq!(output.status.code(), Some(1), "{package}");
        assert!(text(&output.stderr).ends_with(&error), "{}", text(&output.stderr));
    }
    assert_eq!(listing(&fixture.data.join("app")), Vec::<String>::new());
    assert!(!fixture.data.join("data").exists());
}

#[test]
fn install_from_names_the_source_and_a_disabled_source_is_refused() {
    let dir = scratch("from");
    let fixture = repository(&dir, &catalog());

    let unknown = uxdroid(
        &fixture.data,
        &["install", "com.example.notes", "--from", "nope"],
    );
    let disabled = uxdroid(
        &fixture.data,
        &["install", "com.example.notes", "--from", "fdroid"],
    );
    let missing = uxdroid(
        &fixture.data,
        &["install", "com.example.none", "--from", "test"],
    );
    let named = uxdroid(
        &fixture.data,
        &["install", "com.example.notes", "--from", "test"],
    );

    assert_eq!(
        text(&unknown.stderr),
        "error: there is no source named nope: uxdroid sources lists them\n"
    );
    assert_eq!(
        text(&disabled.stderr),
        "error: the source fdroid is disabled: uxdroid sources enable fdroid\n"
    );
    assert!(text(&missing.stderr).ends_with("\nerror: com.example.none is not in test\n"));
    for output in [&unknown, &disabled, &missing] {
        assert_eq!(output.status.code(), Some(1));
    }
    assert!(ok(&named).starts_with("downloaded com.example.notes 2.0 (2) from test:"));
}

#[test]
fn update_installs_a_newer_version_from_the_apps_source_and_does_nothing_when_current() {
    let dir = scratch("update");
    let key = Key::test();
    let fixture = repository(&dir, &catalog());
    ok(&uxdroid(&fixture.data, &["install", "com.example.notes"]));
    ok(&uxdroid(
        &fixture.data,
        &["install", "com.example.markdown"],
    ));

    let current = uxdroid(&fixture.data, &["update"]);
    let mut newer = catalog();
    let notes = "com.example.notes";
    newer[1].versions.push(Version::new(
        notes,
        6,
        "6.0",
        apk(notes, 6, "6.0", &key),
        &key,
    ));
    fdroid::write_repo(&fixture.repo, SECOND, &newer, &[], &key);
    let updated = uxdroid(&fixture.data, &["update", "--refresh"]);

    assert_eq!(
        ok(&current),
        "com.example.markdown 1.0 (1) from test is up to date\n\
         com.example.notes 2.0 (2) from test is up to date\n"
    );
    assert_eq!(
        ok(&updated),
        format!(
            "com.example.markdown 1.0 (1) from test is up to date\n\
             downloaded com.example.notes 6.0 (6) from test: {} bytes, SHA-256 {}\n\
             installed com.example.notes: 0 native libraries for x86_64 in {}\n\
             updated com.example.notes from 2.0 (2) to 6.0 (6) from test\n",
            newer[1].versions[5].apk.len(),
            sha256_hex(&newer[1].versions[5].apk),
            fixture.data.join("data/com.example.notes/lib").display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("app/.com.example.notes.source")).unwrap(),
        "{\"source\":\"test\",\"version_code\":6,\"version_name\":\"6.0\"}\n"
    );
    let not_installed = uxdroid(&fixture.data, &["update", "com.example.none"]);
    assert_eq!(not_installed.status.code(), Some(1));
    assert_eq!(
        text(&not_installed.stderr),
        "error: com.example.none is not installed\n"
    );
}

#[test]
fn an_update_signed_by_another_certificate_is_refused_and_the_app_stays() {
    let dir = scratch("update-signer");
    let fixture = repository(&dir, &catalog());
    ok(&uxdroid(&fixture.data, &["install", "com.example.notes"]));
    let signer = std::fs::read(fixture.data.join("app/.com.example.notes.signer")).unwrap();
    let other = Key::other();
    let notes = "com.example.notes";
    let mut newer = catalog();
    newer[1].versions.push(Version::new(
        notes,
        6,
        "6.0",
        apk(notes, 6, "6.0", &other),
        &other,
    ));
    fdroid::write_repo(&fixture.repo, SECOND, &newer, &[], &Key::test());

    let output = uxdroid(&fixture.data, &["update", "com.example.notes", "--refresh"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains(&format!(
            "error: cannot update com.example.notes: com.example.notes is installed signed by a different certificate, so this update is refused: installed {}, update {}\n",
            Key::test().fingerprint(),
            other.fingerprint()
        )),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(
        std::fs::read(fixture.data.join("app/.com.example.notes.signer")).unwrap(),
        signer
    );
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("app/.com.example.notes.source")).unwrap(),
        "{\"source\":\"test\",\"version_code\":2,\"version_name\":\"2.0\"}\n"
    );
}

#[test]
fn remove_deletes_the_apps_code_data_signer_record_and_component_settings() {
    let dir = scratch("remove");
    let fixture = repository(&dir, &catalog());
    let data = &fixture.data;
    ok(&uxdroid(data, &["install", "com.example.notes"]));
    ok(&uxdroid(data, &["install", "com.example.markdown"]));
    std::fs::create_dir_all(data.join("app/com.example.notes")).unwrap();
    std::fs::write(data.join("app/com.example.notes/base.apk"), b"apk").unwrap();
    std::fs::create_dir_all(data.join("data/com.example.notes/databases")).unwrap();
    std::fs::write(
        data.join("data/com.example.notes/databases/notes.db"),
        b"db",
    )
    .unwrap();
    std::fs::create_dir_all(data.join("system/components")).unwrap();
    std::fs::write(
        data.join("system/components/com.example.notes"),
        b"2 .Main\n",
    )
    .unwrap();
    std::fs::write(
        data.join("system/components/com.example.markdown"),
        b"2 .Main\n",
    )
    .unwrap();

    let output = uxdroid(data, &["remove", "com.example.notes"]);

    assert_eq!(
        ok(&output),
        "removed com.example.notes: its code, data, signer record and component settings\n"
    );
    assert_eq!(
        app_files(data, "com.example.notes"),
        [".com.example.notes.lock"]
    );
    assert_eq!(listing(&data.join("data")), ["com.example.markdown"]);
    assert_eq!(
        listing(&data.join("system/components")),
        ["com.example.markdown"]
    );
    assert_eq!(
        ok(&uxdroid(data, &["list"])),
        "PACKAGE               SOURCE  VERSION\ncom.example.markdown  test    1.0 (1)\n"
    );
    let escape = uxdroid(data, &["remove", "../data"]);
    assert_eq!(escape.status.code(), Some(1));
    assert_eq!(text(&escape.stderr), "error: bad package name ../data\n");
    assert!(data.join("data/com.example.markdown").exists());
    let again = uxdroid(data, &["remove", "com.example.notes"]);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(
        text(&again.stderr),
        format!(
            "error: com.example.notes is not installed in {}\n",
            data.display()
        )
    );
}

#[test]
fn a_running_app_is_not_removed() {
    let dir = scratch("remove-running");
    let fixture = repository(&dir, &catalog());
    let data = &fixture.data;
    ok(&uxdroid(data, &["install", "com.example.notes"]));
    let running = std::fs::File::create(data.join("app/.com.example.notes.running")).unwrap();
    running.lock().unwrap();

    let output = uxdroid(data, &["remove", "com.example.notes"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "error: com.example.notes is running, so it is not removed: uxdroid app com.example.notes stop stops it\n"
    );
    assert!(data.join("app/.com.example.notes.signer").exists());
    assert!(data.join("data/com.example.notes/abi").exists());
}

#[test]
fn an_apk_path_installs_as_before_and_ends_the_apps_tie_to_its_source() {
    let dir = scratch("path");
    let fixture = repository(&dir, &catalog());
    let data = &fixture.data;
    ok(&uxdroid(data, &["install", "com.example.notes"]));
    // A file named like a package is still a file.
    let file = dir.join("com.example.notes");
    std::fs::write(&file, apk("com.example.notes", 7, "7.0", &Key::test())).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .current_dir(&dir)
        .args(["install", "com.example.notes", "--data"])
        .arg(data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "/nonexistent")
        .output()
        .unwrap();

    assert_eq!(
        ok(&output),
        format!(
            "installed com.example.notes: 0 native libraries for x86_64 in {}\n",
            data.join("data/com.example.notes/lib").display()
        )
    );
    assert!(text(&output.stderr).starts_with("note: no framework in /nonexistent/uxdroid/runtime"));
    assert_eq!(
        app_files(data, "com.example.notes"),
        [".com.example.notes.lock", ".com.example.notes.signer"]
    );
    assert!(file.exists(), "the file given stays where it was");
    let update = uxdroid(data, &["update", "com.example.notes"]);
    assert_eq!(update.status.code(), Some(1));
    assert_eq!(
        text(&update.stderr),
        "error: com.example.notes was installed from a file, so it has no source to update from: uxdroid install com.example.notes --from <source> installs it from one\n"
    );
}

/// Runs uxdroid as `uxdroid` does, but gives up after `seconds`: `None` when it had not finished.
fn uxdroid_within(data: &Path, args: &[&str], seconds: u64) -> Option<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uxdroid"));
    command
        .args(args)
        .arg("--data")
        .arg(data)
        .env("UXDROID_RUNTIME", "")
        .env("XDG_DATA_HOME", "/nonexistent")
        .env_remove("http_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("all_proxy")
        .env_remove("ALL_PROXY")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn a_stalled_server_is_given_up_on_within_a_time_bound_by_what_it_may_send() {
    let dir = scratch("stall");
    let fixture = repository(&dir, &catalog());
    fixture.server.behave("/repo/entry.jar", Behavior::Stall);
    let started = std::time::Instant::now();

    let output = uxdroid_within(&fixture.data, &["search", "notes"], 40)
        .expect("the search gave up on the stalled server");

    // The entry may have 256 KiB: 10 s plus that at 128 KiB/s.
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: cannot fetch {}/entry.jar: it did not finish within 12 s, so it was stopped\n",
            fixture.server.url()
        )
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn a_fresh_cache_is_read_while_another_command_holds_the_refresh_lock() {
    let dir = scratch("lockless");
    let fixture = repository(&dir, &catalog());
    let first = ok(&uxdroid(&fixture.data, &["search", "notes"]));
    let refreshing = std::fs::File::create(fixture.data.join("store/sources/test/.lock")).unwrap();
    refreshing.lock().unwrap();

    let output = uxdroid_within(&fixture.data, &["search", "notes"], 10)
        .expect("the search did not wait for the refresh lock");

    assert_eq!(ok(&output), first);
}

/// A data directory whose only enabled source is a repository nobody serves.
fn unreachable_source(dir: &Path) -> (PathBuf, String) {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/repo");
    let data = dir.join("data");
    ok(&uxdroid(&data, &["sources", "disable", "fdroid"]));
    ok(&uxdroid(
        &data,
        &[
            "sources",
            "add",
            "gone",
            &url,
            "--fingerprint",
            &Key::test().fingerprint(),
        ],
    ));
    (data, url)
}

#[test]
fn a_word_that_names_no_file_is_looked_up_as_a_package_and_the_user_is_told() {
    let dir = scratch("not-a-file");
    let (data, url) = unreachable_source(&dir);

    let output = uxdroid(&data, &["install", "com.example.typo"]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(&format!(
            "note: com.example.typo is not a file, so it is looked up as a package name in the enabled sources\nerror: cannot fetch {url}/entry.jar: "
        )),
        "{stderr}"
    );
    // Named as a package, it needs no explanation.
    let named = uxdroid(&data, &["install", "com.example.typo", "--from", "gone"]);
    assert!(
        text(&named.stderr).starts_with("error: cannot fetch"),
        "{}",
        text(&named.stderr)
    );
}

#[test]
fn a_first_lookup_that_fails_leaves_no_store_state() {
    let dir = scratch("no-state");
    let (data, _) = unreachable_source(&dir);
    let fresh = dir.join("fresh");
    std::fs::create_dir_all(fresh.join("store")).unwrap();
    std::fs::copy(
        data.join("store/sources.json"),
        fresh.join("store/sources.json"),
    )
    .unwrap();

    let install = uxdroid(&data, &["install", "com.example.none"]);
    let search = uxdroid(&fresh, &["search", "notes"]);

    assert_eq!(install.status.code(), Some(1));
    assert_eq!(search.status.code(), Some(1));
    assert_eq!(listing(&data.join("store")), [".lock", "sources.json"]);
    assert_eq!(listing(&fresh.join("store")), ["sources.json"]);
}

#[test]
fn a_repository_over_plain_http_is_refused_unless_it_is_on_this_machine() {
    let data = scratch("http").join("data");
    let fingerprint = Key::test().fingerprint();

    let remote = uxdroid(
        &data,
        &[
            "sources",
            "add",
            "plain",
            "http://example.org/repo",
            "--fingerprint",
            &fingerprint,
        ],
    );

    assert_eq!(remote.status.code(), Some(1));
    assert_eq!(
        text(&remote.stderr),
        "error: http://example.org/repo is not a repository address: give an https URL without a query (plain http only for this machine, such as http://127.0.0.1)\n"
    );
    for (name, url) in [
        ("loopback", "http://127.0.0.1:8080/repo"),
        ("localhost", "http://localhost/repo"),
        ("six", "http://[::1]:8080/repo"),
        ("secure", "https://example.org/repo"),
    ] {
        ok(&uxdroid(
            &data,
            &["sources", "add", name, url, "--fingerprint", &fingerprint],
        ));
    }
    let lookalike = uxdroid(
        &data,
        &[
            "sources",
            "add",
            "fake",
            "http://127.0.0.1.example.org/repo",
            "--fingerprint",
            &fingerprint,
        ],
    );
    assert_eq!(lookalike.status.code(), Some(1));
    // A user name before `@` must not make another host look like this machine.
    for url in [
        "http://127.0.0.1@example.org/repo",
        "http://localhost:80@example.org/repo",
        "https://user@example.org/repo",
    ] {
        let output = uxdroid(
            &data,
            &[
                "sources",
                "add",
                "userinfo",
                url,
                "--fingerprint",
                &fingerprint,
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{url}");
    }
}

#[test]
fn a_plain_http_repository_that_redirects_is_refused() {
    let dir = scratch("redirect");
    let fixture = repository(&dir, &catalog());
    fixture.server.behave(
        "/repo/entry.jar",
        Behavior::Redirect(format!("{}/index-v2.json", fixture.server.url())),
    );

    let output = uxdroid(&fixture.data, &["search", "notes"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!(
            "error: cannot fetch {}/entry.jar: it redirects, and a plain http address may not\n",
            fixture.server.url()
        )
    );
    assert_eq!(fixture.server.take(), ["/repo/entry.jar"]);
}
