//! The terminal UI driven by keys, with the real worker and store over a repository made by the
//! testkit and read from disk, drawn on ratatui's test backend. No test needs the network.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use uxdroid_store::fetch::Fetch;
use uxdroid_store::{Error, Installed, Refresh, Store};
use uxdroid_testkit::fdroid::{self, App, Version};
use uxdroid_testkit::{Apk, Key, app_manifest, dex};

use super::state::State;
use super::worker::{self, Job, Reply};
use super::{Session, event_loop, view};

const URL: &str = "http://127.0.0.1/repo";
const UPDATED: u64 = 1_793_273_970_519; // 2026-10-29
const FIRST: u64 = 1_793_000_000_000;
const SECOND: u64 = 1_793_100_000_000;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("uxdroid-tui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Serves the files of a directory for `URL`, as the test HTTP server of the store tests does.
struct Files {
    root: PathBuf,
}

impl Fetch for Files {
    fn copy(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, Error> {
        let name = url
            .strip_prefix(URL)
            .ok_or_else(|| Error::new(format!("cannot fetch {url}")))?;
        let bytes = std::fs::read(self.root.join(name.trim_start_matches('/')))
            .map_err(|_| Error::new(format!("cannot fetch {url}: the server answered 404")))?;
        if bytes.len() as u64 > limit {
            return Err(Error::new(format!(
                "{url} is larger than the {limit} bytes allowed, so it is refused"
            )));
        }
        for chunk in bytes.chunks(512) {
            sink.write_all(chunk)
                .map_err(|error| Error::new(format!("cannot store {url}: {error}")))?;
        }
        Ok(bytes.len() as u64)
    }
}

fn apk(package: &'static str, code: u32, name: &'static str) -> Vec<u8> {
    Apk::new()
        .manifest(&app_manifest(package, code, name))
        .file("classes.dex", &dex(1, 1))
        .signed_bytes(&Key::test())
}

/// Notes at `newest` and 1.0, Markdown Viewer mentioning notes, and Future Notes, which fits no
/// SDK here.
fn catalog(newest: (u64, &'static str)) -> Vec<App> {
    let key = Key::test();
    let notes = "com.example.notes";
    let viewer = "com.example.markdown";
    let future = "com.example.future";
    let mut only_old = Version::new(future, 1, "1.0", apk(future, 1, "1.0"), &key);
    only_old.max_sdk = Some(30);
    vec![
        App::new(
            viewer,
            "Markdown Viewer",
            "Reads notes and documents",
            UPDATED,
        )
        .version(Version::new(viewer, 1, "1.0", apk(viewer, 1, "1.0"), &key)),
        App::new(notes, "Notes", "Write markdown", UPDATED)
            .version(Version::new(
                notes,
                newest.0,
                newest.1,
                apk(notes, newest.0 as u32, newest.1),
                &key,
            ))
            .version(Version::new(notes, 1, "1.0", apk(notes, 1, "1.0"), &key)),
        App::new(future, "Future Notes", "Not yet", UPDATED).version(only_old),
    ]
}

/// A data directory whose only enabled source is the repository in `dir/repo`.
fn repository(dir: &Path) -> PathBuf {
    fdroid::write_repo(
        &dir.join("repo"),
        FIRST,
        &catalog((2, "2.0")),
        &[],
        &Key::test(),
    );
    let data = dir.join("data");
    Store::open(&data)
        .unwrap()
        .change(|config| {
            config.set_enabled("fdroid", false)?;
            config
                .add("test", URL, &Key::test().fingerprint())
                .map(|_| ())
        })
        .unwrap();
    data
}

struct Ui {
    state: State,
    terminal: Terminal<TestBackend>,
    jobs: Sender<Job>,
    replies: Receiver<Reply>,
    transfers: usize,
}

impl Ui {
    fn new(dir: &Path, data: &Path, width: u16, height: u16) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let root = dir.join("repo");
        let connect: worker::Connect =
            Arc::new(move || Box::new(Files { root: root.clone() }) as Box<dyn Fetch>);
        let (jobs, replies) = worker::spawn(data.to_path_buf(), connect, cancel.clone());
        let mut ui = Self {
            state: State::new(data.to_path_buf(), cancel),
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            jobs,
            replies,
            transfers: 0,
        };
        ui.settle();
        ui
    }

    fn press(&mut self, code: KeyCode) {
        self.state.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(&mut self, text: &str) {
        text.chars().for_each(|c| self.press(KeyCode::Char(c)));
    }

    fn send(&mut self) {
        for job in self.state.take_jobs() {
            self.jobs.send(job).unwrap();
        }
    }

    /// Runs the jobs asked for until the worker is idle again.
    fn settle(&mut self) {
        self.send();
        while self.state.busy.is_some() {
            let reply = self
                .replies
                .recv_timeout(Duration::from_secs(60))
                .expect("the worker answers");
            if let Reply::Transfer(..) = reply {
                self.transfers += 1;
            }
            self.state.reply(reply);
            self.send();
        }
    }

    fn screen(&mut self) -> String {
        self.terminal
            .draw(|frame| view::draw(frame, &self.state))
            .unwrap();
        let buffer = self.terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

fn line_with<'a>(screen: &'a str, text: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(text))
        .unwrap_or_else(|| panic!("no line has {text:?}:\n{screen}"))
}

#[test]
fn typing_searches_the_cached_index_as_the_user_types() {
    let dir = scratch("search");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    let screen = ui.screen();
    assert!(screen.contains("type to search 3 apps"), "{screen}");

    ui.typed("notes");
    // No job: the search runs over the apps already read.
    assert!(ui.state.take_jobs().is_empty());
    let screen = ui.screen();
    let notes = line_with(&screen, "com.example.notes");
    for column in ["Notes", "test", "2.0", "2026-10-29"] {
        assert!(notes.contains(column), "{notes}");
    }
    assert!(!notes.contains('✓'), "not installed: {notes}");
    assert!(
        screen.contains("NAME") && screen.contains("UPDATED"),
        "{screen}"
    );
    assert!(
        screen.contains("2 of 3 apps (1 more without a version here)"),
        "{screen}"
    );
    let first = screen.find("com.example.notes").unwrap();
    assert!(
        first < screen.find("com.example.markdown").unwrap(),
        "{screen}"
    );

    ui.typed("zz");
    let screen = ui.screen();
    assert!(screen.contains("no app matches noteszz"), "{screen}");
    ui.press(KeyCode::Backspace);
    ui.press(KeyCode::Backspace);
    assert!(ui.screen().contains("com.example.notes"));

    // Out of the query box, ? shows the keys.
    ui.press(KeyCode::Esc);
    ui.press(KeyCode::Char('?'));
    let screen = ui.screen();
    assert!(
        screen.contains("Keys") && screen.contains("install the selected app"),
        "{screen}"
    );
}

#[test]
fn enter_shows_the_details_of_the_selected_app() {
    let dir = scratch("details");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    ui.typed("notes");
    ui.press(KeyCode::Enter);
    ui.press(KeyCode::Enter);
    let screen = ui.screen();
    for text in [
        "Details",
        "com.example.notes from test",
        "version 2.0 (2), updated 2026-10-29",
        "license Apache-2.0, size ",
        "not installed; i installs it",
        "Write markdown",
        "permissions (1):",
        "android.permission.INTERNET",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }
    // Esc goes back to the results; j selects the next one.
    ui.press(KeyCode::Esc);
    ui.press(KeyCode::Char('j'));
    ui.press(KeyCode::Enter);
    assert!(ui.screen().contains("com.example.markdown from test"));

    // A wide terminal shows the details beside the results.
    let mut wide = Ui::new(&dir, &data, 140, 30);
    wide.typed("notes");
    let screen = wide.screen();
    let row = line_with(&screen, "2026-10-29");
    assert!(row.contains("com.example.notes"), "{screen}");
    assert!(screen.contains("license Apache-2.0"), "{screen}");
}

#[test]
fn i_installs_the_selected_app_from_the_repository_showing_progress() {
    let dir = scratch("install");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    ui.typed("notes");
    ui.press(KeyCode::Enter);
    ui.press(KeyCode::Char('i'));
    assert!(
        ui.screen()
            .contains("installing com.example.notes from test")
    );
    ui.settle();
    assert!(ui.transfers > 0, "the download reported its bytes");
    let screen = ui.screen();
    assert!(!ui.state.status.error, "{screen}");
    assert!(
        line_with(&screen, "com.example.notes").contains('✓'),
        "{screen}"
    );
    assert_eq!(
        Installed::read(&data, "com.example.notes").unwrap(),
        Some(Installed {
            source: "test".to_string(),
            version_code: 2,
            version_name: "2.0".to_string(),
        })
    );
    assert!(data.join("data/com.example.notes/abi").is_file());

    // Installing it again is a no-op the status line says.
    ui.press(KeyCode::Char('i'));
    ui.settle();
    assert!(
        ui.screen()
            .contains("com.example.notes 2.0 (2) from test is already installed")
    );
}

#[test]
fn installed_shows_updates_and_update_all_installs_them() {
    let dir = scratch("update");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    ui.typed("notes");
    ui.press(KeyCode::Enter);
    ui.press(KeyCode::Char('i'));
    ui.settle();
    ui.press(KeyCode::Char('2'));
    let screen = ui.screen();
    let row = line_with(&screen, "com.example.notes");
    assert!(row.contains("test") && row.contains("2.0 (2)") && row.contains("up to date"));
    assert!(screen.contains("1 installed, 0 updates"), "{screen}");

    fdroid::write_repo(
        &dir.join("repo"),
        SECOND,
        &catalog((3, "3.0")),
        &[],
        &Key::test(),
    );
    ui.press(KeyCode::Char('r'));
    ui.settle();
    let screen = ui.screen();
    assert!(
        line_with(&screen, "com.example.notes").contains("3.0 (3)"),
        "{screen}"
    );
    assert!(screen.contains("1 installed, 1 update"), "{screen}");

    ui.press(KeyCode::Char('U'));
    ui.settle();
    let screen = ui.screen();
    assert!(
        screen.contains("updated com.example.notes from 2.0 (2) to 3.0 (3) from test"),
        "{screen}"
    );
    assert!(line_with(&screen, "com.example.notes").contains("up to date"));
    assert_eq!(
        Installed::read(&data, "com.example.notes")
            .unwrap()
            .unwrap()
            .version_code,
        3
    );
}

#[test]
fn remove_asks_first_and_keeps_the_app_unless_confirmed() {
    let dir = scratch("remove");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    ui.typed("notes");
    ui.press(KeyCode::Enter);
    ui.press(KeyCode::Char('d'));
    assert!(ui.screen().contains("com.example.notes is not installed"));
    ui.press(KeyCode::Char('i'));
    ui.settle();

    ui.press(KeyCode::Char('d'));
    let screen = ui.screen();
    assert!(
        screen.contains("Remove com.example.notes with its data"),
        "{screen}"
    );
    assert!(
        ui.state.take_jobs().is_empty(),
        "nothing runs before an answer"
    );
    // Other keys leave the question open; n keeps the app.
    ui.press(KeyCode::Char('x'));
    assert!(ui.screen().contains("Remove com.example.notes"));
    ui.press(KeyCode::Char('n'));
    assert!(ui.screen().contains("kept com.example.notes"));
    assert!(ui.state.take_jobs().is_empty());
    assert!(data.join("data/com.example.notes").is_dir());

    ui.press(KeyCode::Char('2'));
    ui.press(KeyCode::Char('d'));
    ui.press(KeyCode::Char('y'));
    ui.settle();
    let screen = ui.screen();
    assert!(
        screen.contains(
            "removed com.example.notes: its code, data, signer record and component settings"
        ),
        "{screen}"
    );
    assert!(screen.contains("no app is installed in"), "{screen}");
    assert!(!data.join("data/com.example.notes").exists());
    assert_eq!(Installed::read(&data, "com.example.notes").unwrap(), None);
}

#[test]
fn sources_are_added_with_the_commands_validation_and_can_be_switched_and_timed() {
    let dir = scratch("sources");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 80, 24);
    ui.press(KeyCode::Tab);
    ui.press(KeyCode::Tab);
    ui.press(KeyCode::Tab);
    let screen = ui.screen();
    let fdroid = line_with(&screen, "https://f-droid.org/repo");
    assert!(
        fdroid.contains("fdroid") && fdroid.contains("no"),
        "{screen}"
    );
    assert!(line_with(&screen, URL).contains("yes"), "{screen}");
    assert!(screen.contains("indexes are refreshed when older than 24 hours"));

    ui.press(KeyCode::Char('a'));
    ui.typed("two");
    ui.press(KeyCode::Tab);
    ui.typed("ftp://example.org/repo");
    ui.press(KeyCode::Tab);
    ui.typed(&Key::test().fingerprint());
    ui.press(KeyCode::Enter);
    ui.settle();
    let screen = ui.screen();
    assert!(
        screen.contains("ftp://example.org/repo is not a repository address"),
        "{screen}"
    );
    let config = Store::open(&data).unwrap().config().clone();
    assert!(config.source("two").is_none(), "nothing was added");

    // The form keeps what was typed; the URL is corrected.
    ui.press(KeyCode::Up);
    for _ in "ftp://example.org/repo".chars() {
        ui.press(KeyCode::Backspace);
    }
    ui.typed("https://example.org/repo");
    ui.press(KeyCode::Enter);
    ui.press(KeyCode::Enter);
    ui.settle();
    let screen = ui.screen();
    assert!(!screen.contains("Add an F-Droid repository"), "{screen}");
    assert!(screen.contains("added two"), "{screen}");
    let row = line_with(&screen, "https://example.org/repo");
    assert!(row.contains("two") && row.contains("yes"), "{screen}");
    let config = Store::open(&data).unwrap().config().clone();
    assert_eq!(
        config.source("two").unwrap().url,
        "https://example.org/repo"
    );

    ui.press(KeyCode::End);
    ui.press(KeyCode::Char('e'));
    ui.settle();
    let screen = ui.screen();
    assert!(screen.contains("disabled two"), "{screen}");
    assert!(line_with(&screen, "https://example.org/repo").contains("no"));

    ui.press(KeyCode::Char('h'));
    ui.press(KeyCode::Backspace);
    ui.press(KeyCode::Backspace);
    ui.typed("9999");
    ui.press(KeyCode::Enter);
    assert!(
        ui.screen()
            .contains("9999 is not an interval: give whole hours from 0 to 8760")
    );
    for _ in 0..4 {
        ui.press(KeyCode::Backspace);
    }
    ui.typed("48");
    ui.press(KeyCode::Enter);
    ui.settle();
    let screen = ui.screen();
    assert!(
        screen.contains("indexes are refreshed when older than 48 hours"),
        "{screen}"
    );
    let config = Store::open(&data).unwrap().config().clone();
    assert_eq!(config.refresh_after_hours, 48);
}

#[test]
fn a_small_terminal_shows_fewer_columns_and_a_tiny_one_asks_for_room() {
    let dir = scratch("small");
    let data = repository(&dir);
    let mut ui = Ui::new(&dir, &data, 50, 12);
    ui.typed("notes");
    let screen = ui.screen();
    let row = line_with(&screen, "Notes");
    assert!(row.contains("2.0"), "{screen}");
    assert!(!screen.contains("com.example.notes"), "{screen}");
    assert!(screen.contains("/ notes"), "a one-line query box: {screen}");

    let mut tiny = Ui::new(&dir, &data, 30, 8);
    tiny.typed("notes");
    let screen = tiny.screen();
    // The request wraps over lines.
    let words: Vec<&str> = screen.split_whitespace().collect();
    assert!(
        words.join(" ").starts_with(
            "uxdroid tui needs a terminal of at least 40x10; this one is 30x8. q quits."
        ),
        "{screen}"
    );
}

#[test]
fn quitting_waits_for_the_running_job_unless_asked_twice() {
    let dir = scratch("quit");
    let mut state = State::new(dir.join("data"), Arc::new(AtomicBool::new(false)));
    let quit = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    state.key(quit);
    assert!(!state.quit, "the first index read is running");
    state.reply(Reply::Done(Ok(String::new())));
    assert!(state.quit);

    let mut state = State::new(dir.join("data"), Arc::new(AtomicBool::new(false)));
    state.key(quit);
    state.key(quit);
    assert!(state.quit);
}

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

const ENTER: &str = "\u{1b}[?1049h\u{1b}[?25l";
const LEAVE: &str = "\u{1b}[?25h\u{1b}[?1049l";

#[test]
fn the_terminal_is_restored_after_a_quit_an_error_or_a_panic() {
    let dir = scratch("restore");
    let (jobs, _inbox) = std::sync::mpsc::channel();
    let (_outbox, replies) = std::sync::mpsc::channel();
    let loop_with = |events: &mut dyn FnMut(Duration) -> std::io::Result<Option<Event>>| {
        let out = Shared::default();
        let result = {
            let _session = Session::start(out.clone(), false).unwrap();
            let mut state = State::new(dir.join("data"), Arc::new(AtomicBool::new(false)));
            state.reply(Reply::Done(Ok(String::new())));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            event_loop(&mut terminal, &mut state, &jobs, &replies, events)
        };
        (result, out.text())
    };

    let mut keys = vec![
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
    ]
    .into_iter();
    let (result, out) = loop_with(&mut |_| Ok(keys.next()));
    assert!(result.is_ok());
    assert_eq!(out, format!("{ENTER}{LEAVE}"));

    let (result, out) = loop_with(&mut |_| Err(std::io::Error::other("the terminal went away")));
    assert_eq!(result.unwrap_err().to_string(), "the terminal went away");
    assert_eq!(out, format!("{ENTER}{LEAVE}"));

    let out = Shared::default();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _session = Session::start(out.clone(), false).unwrap();
        panic!("a bug");
    }));
    assert!(panicked.is_err());
    assert_eq!(out.text(), format!("{ENTER}{LEAVE}"));
}

#[test]
fn the_first_job_reads_the_cached_indexes_without_refreshing_them() {
    let state = State::new(
        PathBuf::from("/nonexistent"),
        Arc::new(AtomicBool::new(false)),
    );
    let mut state = state;
    assert_eq!(state.take_jobs(), vec![Job::Load(Refresh::IfStale)]);
}
