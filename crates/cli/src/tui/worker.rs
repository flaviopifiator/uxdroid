//! The thread that does what the terminal UI asks: reading indexes, downloading, installing,
//! removing, changing sources and starting apps, through the same functions the commands use. The UI thread only
//! sends jobs and reads replies, so it never waits on the network or an install.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use uxdroid_store::config::Config;
use uxdroid_store::fetch::{Fetch, Http};
use uxdroid_store::{App, Permissions, Refresh, Store, Want};
use uxdroid_system::control::protocol::Intent;
use uxdroid_system::control::{DEFAULT_PROFILE, control_dir};

use crate::app::{self, Done, Instance};
use crate::store::{Listed, install_package, list, remove_package, update};

/// Makes the transport each job's store fetches with: HTTP, or files in tests.
pub(crate) type Connect = Arc<dyn Fn() -> Box<dyn Fetch> + Send + Sync>;

/// The worker thread's name, which its panics are known by.
pub(crate) const NAME: &str = "uxdroid-tui-worker";

pub(crate) fn http() -> Connect {
    Arc::new(|| Box::new(Http::new()))
}

/// Finds the control directory apps started from the UI are reached in: the user's, or one in tests.
pub(crate) type Control = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

pub(crate) fn control() -> Control {
    Arc::new(control_dir)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    /// Reads the installed apps, the sources and the indexes of the enabled sources.
    Load(Refresh),
    Install {
        package: String,
        source: String,
    },
    /// One app, or all installed from a source.
    Update(Option<String>),
    Remove(String),
    AddSource {
        name: String,
        url: String,
        fingerprint: String,
    },
    Enable {
        name: String,
        enabled: bool,
    },
    RefreshAfter(u64),
    /// Asks Google Play, which has no index to search as the user types, for what was typed.
    Search(String),
    /// Starts an installed app, or hands the start to it when it runs.
    Start(String),
}

impl Job {
    /// What the UI says while it runs.
    pub(crate) fn describe(&self) -> String {
        match self {
            Job::Load(Refresh::IfStale) => "reading the indexes".to_string(),
            Job::Load(Refresh::Always) => "refreshing the indexes".to_string(),
            Job::Install { package, source } => format!("installing {package} from {source}"),
            Job::Update(Some(package)) => format!("updating {package}"),
            Job::Update(None) => "updating every app installed from a source".to_string(),
            Job::Remove(package) => format!("removing {package}"),
            Job::AddSource { name, .. } => format!("adding the source {name}"),
            Job::Enable { name, enabled } => {
                format!("{} {name}", if *enabled { "enabling" } else { "disabling" })
            }
            Job::RefreshAfter(_) => "changing the refresh interval".to_string(),
            Job::Search(text) => format!("asking Google Play for {text}"),
            Job::Start(package) => format!("starting {package}"),
        }
    }
}

/// What the worker reports. Every job ends with one `Done`, after the replies it causes.
pub(crate) enum Reply {
    /// A line about a long download, as the commands print it.
    Note(String),
    /// Bytes of an APK downloaded so far, and its size.
    Transfer(u64, u64),
    /// The installed apps and the sources, read from the data directory.
    Local {
        installed: Vec<Listed>,
        config: Result<Config, String>,
    },
    /// The apps of the enabled sources and notes about them, or why none could be read.
    Apps(Result<(Vec<App>, Vec<String>), String>),
    /// The apps Google Play found for a search, and notes about it, or why it could not be asked.
    Found(Result<(Vec<App>, Vec<String>), String>),
    /// What each version of those apps requests, as their cached indexes list it.
    Permissions(Permissions),
    Done(Result<String, String>),
    /// The worker panicked with this message and stopped.
    Failed(String),
}

/// Starts the worker for `data`. Setting `cancel` stops the download in progress; the worker clears
/// it when a job starts.
pub(crate) fn spawn(
    data: PathBuf,
    connect: Connect,
    control: Control,
    cancel: Arc<AtomicBool>,
) -> (Sender<Job>, Receiver<Reply>) {
    let (jobs, inbox) = channel::<Job>();
    let (outbox, replies) = channel();
    std::thread::Builder::new()
        .name(NAME.to_string())
        .spawn(move || {
            for job in inbox {
                cancel.store(false, Ordering::SeqCst);
                let done = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    perform(&data, &connect, &control, &cancel, &outbox, job)
                }));
                let reply = match done {
                    Ok(done) => Reply::Done(done),
                    Err(panic) => {
                        let message = panic
                            .downcast_ref::<&str>()
                            .map(|message| message.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "a panic without a message".to_string());
                        let _ = outbox.send(Reply::Failed(message));
                        break;
                    }
                };
                if outbox.send(reply).is_err() {
                    break;
                }
            }
        })
        .expect("the worker thread starts");
    (jobs, replies)
}

/// The store of `data`, reporting its notes and transfers to the UI.
fn open(
    data: &Path,
    connect: &Connect,
    cancel: &Arc<AtomicBool>,
    outbox: &Sender<Reply>,
) -> Result<Store, String> {
    let mut store = Store::open_with(data, connect()).map_err(|error| error.0)?;
    store.set_profile(crate::store::profile());
    let notes = outbox.clone();
    store.on_progress(Box::new(move |message| {
        let _ = notes.send(Reply::Note(message.to_string()));
    }));
    let transfers = outbox.clone();
    let cancel = cancel.clone();
    // A reply for each percent at most, so a fast download does not flood the UI.
    let last = Cell::new(u64::MAX);
    store.on_transfer(Box::new(move |done, total| {
        let percent = done.saturating_mul(100) / total.max(1);
        if percent != last.get() {
            last.set(percent);
            let _ = transfers.send(Reply::Transfer(done, total));
        }
        !cancel.load(Ordering::SeqCst)
    }));
    Ok(store)
}

fn local(data: &Path, outbox: &Sender<Reply>) {
    let config = Config::load(&data.join("store/sources.json")).map_err(|error| error.0);
    let _ = outbox.send(Reply::Local {
        installed: list(data),
        config,
    });
}

/// The apps, then their permissions, which only the details show, so the results come first.
fn apps(store: &Store, refresh: Refresh, outbox: &Sender<Reply>) {
    let _ = outbox.send(Reply::Apps(
        store.apps(None, refresh).map_err(|error| error.0),
    ));
    let _ = outbox.send(Reply::Permissions(store.permissions()));
}

/// Starts `package` as `uxdroid app <package> start --data-dir <data>` does.
fn start(data: &Path, control: &Control, package: &str) -> Result<String, String> {
    let app = Instance {
        package: package.to_string(),
        profile: DEFAULT_PROFILE.to_string(),
    };
    match app::start(
        &control()?,
        &app,
        Intent::default(),
        Some(data.to_path_buf()),
        None,
    )? {
        Done::Printed(report) => Ok(report),
        Done::NotRunning(package, _) => Err(format!("{package} is not running")),
    }
}

fn perform(
    data: &Path,
    connect: &Connect,
    control: &Control,
    cancel: &Arc<AtomicBool>,
    outbox: &Sender<Reply>,
    job: Job,
) -> Result<String, String> {
    // A start needs no store.
    if let Job::Start(package) = &job {
        return start(data, control, package);
    }
    let note = |message: &str| {
        let _ = outbox.send(Reply::Note(message.to_string()));
    };
    let mut store = match open(data, connect, cancel, outbox) {
        Ok(store) => store,
        Err(error) => {
            local(data, outbox);
            return Err(error);
        }
    };
    match job {
        Job::Load(refresh) => {
            local(data, outbox);
            apps(&store, refresh, outbox);
            Ok(String::new())
        }
        Job::Install { package, source } => {
            let result = install_package(
                &store,
                data,
                &package,
                Some(&source),
                Refresh::IfStale,
                &note,
            );
            local(data, outbox);
            result.map(|(report, message)| match message {
                Some(message) => format!("{report}note: {message}\n"),
                None => report,
            })
        }
        Job::Update(package) => {
            let faults = RefCell::new(Vec::new());
            let result = update(
                data,
                package.as_deref(),
                Refresh::IfStale,
                &|| open(data, connect, cancel, outbox),
                &note,
                &|message| faults.borrow_mut().push(message),
            );
            local(data, outbox);
            let faults = faults.into_inner();
            match result {
                Ok(out) if faults.is_empty() => Ok(out),
                Ok(out) => Err(format!("{}\n{out}", faults.join("\n"))),
                Err(error) => Err(error),
            }
        }
        Job::Remove(package) => {
            let result = remove_package(data, &package);
            local(data, outbox);
            result
        }
        Job::AddSource {
            name,
            url,
            fingerprint,
        } => {
            store
                .change(|config| config.add(&name, &url, &fingerprint).map(|_| ()))
                .map_err(|error| error.0)?;
            local(data, outbox);
            apps(&store, Refresh::IfStale, outbox);
            Ok(format!("added {name}"))
        }
        Job::Enable { name, enabled } => {
            store
                .change(|config| config.set_enabled(&name, enabled))
                .map_err(|error| error.0)?;
            local(data, outbox);
            apps(&store, Refresh::IfStale, outbox);
            Ok(format!(
                "{} {name}",
                if enabled { "enabled" } else { "disabled" }
            ))
        }
        Job::Search(text) => {
            let play = store
                .config()
                .play()
                .map(|source| source.name.clone())
                .ok_or_else(|| {
                    "Google Play is not a source: uxdroid sources enable play".to_string()
                })?;
            let found = store
                .apps_for(Some(&play), Refresh::IfStale, Want::Matching(&text))
                .map_err(|error| error.0);
            let failed = found.as_ref().err().cloned();
            let _ = outbox.send(Reply::Found(found));
            match failed {
                Some(error) => Err(error),
                None => Ok(String::new()),
            }
        }
        Job::Start(_) => unreachable!("a start is done before the store opens"),
        Job::RefreshAfter(hours) => {
            store
                .change(|config| {
                    config.refresh_after_hours = hours;
                    Ok(())
                })
                .map_err(|error| error.0)?;
            local(data, outbox);
            Ok(format!(
                "indexes are refreshed when older than {hours} hours"
            ))
        }
    }
}
