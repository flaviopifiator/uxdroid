//! What the terminal UI shows and how keys change it. No terminal and no thread here: keys and the
//! worker's replies come in, jobs for the worker go out, and `view` draws the result.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use uxdroid_store::config::Config;
use uxdroid_store::{App, Device, Permissions, Refresh, Release};

use super::worker::{Job, Reply};
use crate::store::{Listed, clean, refresh_hours};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Search,
    Installed,
    Sources,
}

pub(crate) const VIEWS: [(View, &str); 3] = [
    (View::Search, "Search"),
    (View::Installed, "Installed"),
    (View::Sources, "Sources"),
];

/// A form of text fields, such as the one that adds a source.
pub(crate) struct Form {
    pub kind: FormKind,
    pub fields: Vec<(&'static str, String)>,
    pub focus: usize,
    pub error: Option<String>,
    /// Sent to the worker, waiting for its answer.
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormKind {
    AddSource,
    RefreshAfter,
}

pub(crate) enum Modal {
    Help,
    /// Asks before removing this package.
    Remove(String),
    Form(Form),
}

pub(crate) struct Status {
    pub text: String,
    pub error: bool,
    /// Said while a job runs, so shown beside what it does rather than in place of it.
    pub during_job: bool,
}

pub(crate) struct State {
    pub data: PathBuf,
    pub device: Device,
    pub view: View,
    /// In the search view, whether keys go to the query box.
    pub typing: bool,
    pub query: String,
    /// The apps of the enabled sources, once read.
    pub apps: Option<Vec<App>>,
    /// Indexes into `apps` of the apps matching `query`, best first.
    pub hits: Vec<usize>,
    /// How many more match but have no release for this machine.
    pub unfit: usize,
    pub installed: Vec<Listed>,
    /// What each version requests, once read after the apps.
    pub permissions: Option<Permissions>,
    pub config: Option<Config>,
    /// The selected row of each view.
    pub selected: BTreeMap<&'static str, usize>,
    /// Whether the details of the selected app are open.
    pub details: bool,
    pub modal: Option<Modal>,
    pub status: Status,
    /// Notes about the indexes, such as one that could not be refreshed.
    pub notes: Vec<String>,
    /// What the worker is doing, and how far an APK download is.
    pub busy: Option<String>,
    pub transfer: Option<(u64, u64)>,
    /// Quit once the worker is done.
    pub quit_pending: bool,
    /// The signal that asked the UI to end, which it ends by once the terminal is restored.
    pub signalled: Option<i32>,
    /// The worker's panic, which ends the UI.
    pub failed: Option<String>,
    pub quit: bool,
    pub cancel: Arc<AtomicBool>,
    jobs: Vec<Job>,
}

fn key_name(view: View) -> &'static str {
    match view {
        View::Search => "search",
        View::Installed => "installed",
        View::Sources => "sources",
    }
}

/// The first line of a report that says what happened, for the status line.
fn summary(report: &str) -> String {
    let lines: Vec<&str> = report.lines().filter(|line| !line.is_empty()).collect();
    clean(
        lines
            .iter()
            .rev()
            .find(|line| !line.starts_with("note: "))
            .or(lines.last())
            .copied()
            .unwrap_or_default(),
    )
}

impl State {
    /// The UI for `data`, reading everything in the background from the start.
    pub(crate) fn new(data: PathBuf, cancel: Arc<AtomicBool>) -> Self {
        Self {
            data,
            device: Device::host(),
            view: View::Search,
            typing: true,
            query: String::new(),
            apps: None,
            hits: Vec::new(),
            unfit: 0,
            installed: Vec::new(),
            permissions: None,
            config: None,
            selected: BTreeMap::new(),
            details: false,
            modal: None,
            status: Status {
                text: "type to search; ? shows the keys".to_string(),
                error: false,
                during_job: false,
            },
            notes: Vec::new(),
            busy: Some(Job::Load(Refresh::IfStale).describe()),
            transfer: None,
            quit_pending: false,
            signalled: None,
            failed: None,
            quit: false,
            cancel,
            jobs: vec![Job::Load(Refresh::IfStale)],
        }
    }

    /// The jobs to send to the worker.
    pub(crate) fn take_jobs(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.jobs)
    }

    fn say(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            error: false,
            during_job: self.busy.is_some(),
        };
    }

    fn complain(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            error: true,
            during_job: self.busy.is_some(),
        };
    }

    /// Hands `job` to the worker, unless it is busy: one job runs at a time.
    fn start(&mut self, job: Job) -> bool {
        if let Some(busy) = &self.busy {
            let text = format!("wait: {busy} is still running (Esc cancels a download)");
            self.complain(text);
            return false;
        }
        self.busy = Some(job.describe());
        self.transfer = None;
        self.status.during_job = false;
        self.jobs.push(job);
        true
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected.get(key_name(self.view)).copied().unwrap_or(0)
    }

    fn rows(&self) -> usize {
        match self.view {
            View::Search => self.hits.len(),
            View::Installed => self.installed.len(),
            View::Sources => self
                .config
                .as_ref()
                .map_or(0, |config| config.sources.len()),
        }
    }

    fn select(&mut self, row: usize) {
        let last = self.rows().saturating_sub(1);
        self.selected.insert(key_name(self.view), row.min(last));
    }

    fn move_by(&mut self, delta: isize) {
        let row = self.selected().saturating_add_signed(delta);
        self.select(row);
    }

    /// The app on the selected search row, with the release it would install.
    pub(crate) fn hit(&self) -> Option<(&App, &Release)> {
        let app = &self.apps.as_ref()?[*self.hits.get(self.selected())?];
        Some((app, app.newest(&self.device)?))
    }

    pub(crate) fn listed(&self, package: &str) -> Option<&Listed> {
        self.installed
            .iter()
            .find(|listed| listed.package == package)
    }

    /// The newer release the source of an installed app offers, if any.
    pub(crate) fn available(&self, listed: &Listed) -> Option<&Release> {
        let record = listed.record.as_ref()?;
        self.apps
            .as_ref()?
            .iter()
            .find(|app| app.package == listed.package && app.source == record.source)?
            .newest(&self.device)
            .filter(|release| release.version_code > record.version_code)
    }

    fn search(&mut self) {
        let (hits, unfit) = match &self.apps {
            Some(apps) => {
                let matches = uxdroid_store::search(apps, &self.query, &self.device);
                (
                    matches.hits.iter().map(|hit| hit.index).collect(),
                    matches.unfit,
                )
            }
            None => (Vec::new(), 0),
        };
        self.hits = hits;
        self.unfit = unfit;
        self.select(0);
    }

    pub(crate) fn event(&mut self, event: Event) {
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            self.key(key);
        }
    }

    pub(crate) fn key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return self.request_quit();
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key);
        }
        if control && key.code == KeyCode::Char('r') || key.code == KeyCode::F(5) {
            self.start(Job::Load(Refresh::Always));
            return;
        }
        if let (KeyCode::Esc, Some((done, total))) = (key.code, self.transfer) {
            if done >= total {
                return self
                    .say("the download is complete: the install can no longer be cancelled");
            }
            self.cancel.store(true, Ordering::SeqCst);
            return self.say("cancelling the download");
        }
        if self.view == View::Search && self.typing {
            return self.query_key(key);
        }
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('/') => {
                self.view = View::Search;
                self.typing = true;
                self.details = false;
            }
            KeyCode::Tab => self.cycle(true),
            KeyCode::BackTab => self.cycle(false),
            KeyCode::Char('1') => self.show(View::Search),
            KeyCode::Char('2') => self.show(View::Installed),
            KeyCode::Char('3') => self.show(View::Sources),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Enter if self.view == View::Search => self.details = !self.details,
            KeyCode::Enter | KeyCode::Char('s') if self.view == View::Installed => {
                self.start_selected()
            }
            KeyCode::Esc => self.details = false,
            KeyCode::Char('i') => self.install(),
            KeyCode::Char('u') => self.update_selected(),
            KeyCode::Char('U') => {
                self.start(Job::Update(None));
            }
            KeyCode::Char('d') => self.ask_remove(),
            KeyCode::Char('r') => {
                self.start(Job::Load(Refresh::Always));
            }
            KeyCode::Char('e') | KeyCode::Char(' ') if self.view == View::Sources => {
                self.toggle_source()
            }
            KeyCode::Char('a') if self.view == View::Sources => {
                self.modal = Some(Modal::Form(Form {
                    kind: FormKind::AddSource,
                    fields: vec![
                        ("Name", String::new()),
                        ("URL", String::new()),
                        ("Fingerprint", String::new()),
                    ],
                    focus: 0,
                    error: None,
                    pending: false,
                }))
            }
            KeyCode::Char('h') if self.view == View::Sources => {
                let hours = self
                    .config
                    .as_ref()
                    .map(|config| config.refresh_after_hours.to_string())
                    .unwrap_or_default();
                self.modal = Some(Modal::Form(Form {
                    kind: FormKind::RefreshAfter,
                    fields: vec![("Hours", hours)],
                    focus: 0,
                    error: None,
                    pending: false,
                }))
            }
            _ => {}
        }
    }

    fn show(&mut self, view: View) {
        self.view = view;
        self.typing = false;
        self.details = false;
    }

    /// Tab goes from the query box to the results, then to the next view.
    fn cycle(&mut self, forward: bool) {
        let (view, typing) = match (self.view, self.typing, forward) {
            (View::Search, true, true) => (View::Search, false),
            (View::Search, false, true) => (View::Installed, false),
            (View::Installed, _, true) => (View::Sources, false),
            (View::Sources, _, true) => (View::Search, true),
            (View::Search, true, false) => (View::Sources, false),
            (View::Search, false, false) => (View::Search, true),
            (View::Installed, _, false) => (View::Search, false),
            (View::Sources, _, false) => (View::Installed, false),
        };
        self.view = view;
        self.typing = typing;
        self.details = false;
    }

    fn query_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if control => {
                self.query.clear();
                self.search();
            }
            KeyCode::Char(c) if !control => {
                self.query.push(c);
                self.search();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.search();
            }
            KeyCode::Enter => {
                self.typing = false;
                if self.play_enabled() && !self.query.trim().is_empty() {
                    self.start(Job::Search(self.query.clone()));
                }
            }
            KeyCode::Down | KeyCode::Esc => self.typing = false,
            KeyCode::Tab => self.cycle(true),
            KeyCode::BackTab => self.cycle(false),
            _ => {}
        }
    }

    /// A signal asks the UI to end, as quitting does: once the running job ends, a download in
    /// progress cancelled first; a second signal ends it at once.
    pub(crate) fn signal(&mut self, signal: i32) {
        let again = self.signalled.is_some();
        self.signalled = Some(signal);
        match &self.busy {
            Some(busy) if !again => {
                let text = format!("stopping on signal {signal} when {busy} ends");
                self.cancel.store(true, Ordering::SeqCst);
                self.quit_pending = true;
                self.say(text);
            }
            _ => self.quit = true,
        }
    }

    fn request_quit(&mut self) {
        match &self.busy {
            Some(busy) if !self.quit_pending => {
                let text = format!(
                    "quitting when {busy} ends; Esc cancels a download, q again quits at once"
                );
                self.quit_pending = true;
                self.say(text);
            }
            _ => self.quit = true,
        }
    }

    /// The package of the selected row, in the search or installed view.
    fn package(&self) -> Option<String> {
        match self.view {
            View::Search => self.hit().map(|(app, _)| app.package.clone()),
            View::Installed => self
                .installed
                .get(self.selected())
                .map(|listed| listed.package.clone()),
            View::Sources => None,
        }
    }

    fn install(&mut self) {
        let Some((app, _)) = (self.view == View::Search).then(|| self.hit()).flatten() else {
            return self.complain("select an app in Search to install it");
        };
        let job = Job::Install {
            package: app.package.clone(),
            source: app.source.clone(),
        };
        self.start(job);
    }

    fn update_selected(&mut self) {
        let Some(package) = self.package() else {
            return self.complain("select an installed app to update it; U updates them all");
        };
        if self.listed(&package).is_none() {
            return self.complain(format!("{package} is not installed: i installs it"));
        }
        self.start(Job::Update(Some(package)));
    }

    /// Starts the selected installed app, as `uxdroid app <package> start` does.
    fn start_selected(&mut self) {
        let Some(package) = self.package() else {
            return self.complain("select an installed app to start it");
        };
        self.start(Job::Start(package));
    }

    fn ask_remove(&mut self) {
        let Some(package) = self.package() else {
            return self.complain("select an installed app to remove it");
        };
        if self.listed(&package).is_none() {
            return self.complain(format!("{package} is not installed"));
        }
        self.modal = Some(Modal::Remove(package));
    }

    fn toggle_source(&mut self) {
        let Some(source) = self
            .config
            .as_ref()
            .and_then(|config| config.sources.get(self.selected()))
        else {
            return;
        };
        let job = Job::Enable {
            name: source.name.clone(),
            enabled: !source.enabled,
        };
        self.start(job);
    }

    fn modal_key(&mut self, modal: Modal, key: KeyEvent) {
        match modal {
            Modal::Help => {}
            Modal::Remove(package) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.start(Job::Remove(package));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.say(format!("kept {package}"));
                }
                _ => self.modal = Some(Modal::Remove(package)),
            },
            Modal::Form(form) => self.form_key(form, key),
        }
    }

    fn form_key(&mut self, mut form: Form, key: KeyEvent) {
        if form.pending {
            self.modal = Some(Modal::Form(form));
            return;
        }
        let last = form.fields.len() - 1;
        match key.code {
            KeyCode::Esc => return self.say("nothing changed"),
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => form.focus = form.focus.saturating_sub(1),
            KeyCode::Enter if form.focus < last => form.focus += 1,
            KeyCode::Enter => return self.submit(form),
            KeyCode::Backspace => {
                form.fields[form.focus].1.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                form.fields[form.focus].1.push(c);
            }
            _ => {}
        }
        self.modal = Some(Modal::Form(form));
    }

    fn submit(&mut self, mut form: Form) {
        let value = |index: usize| form.fields[index].1.trim().to_string();
        let job = match form.kind {
            FormKind::AddSource => Job::AddSource {
                name: value(0),
                url: value(1),
                fingerprint: value(2),
            },
            FormKind::RefreshAfter => match refresh_hours(&value(0)) {
                Some(hours) => Job::RefreshAfter(hours),
                None => {
                    form.error = Some(format!(
                        "{} is not an interval: give whole hours from 0 to 8760",
                        clean(&value(0))
                    ));
                    self.modal = Some(Modal::Form(form));
                    return;
                }
            },
        };
        form.pending = self.start(job);
        form.error = None;
        self.modal = Some(Modal::Form(form));
    }

    /// Whether Google Play is an enabled source, which Enter in the query box asks.
    pub(crate) fn play_enabled(&self) -> bool {
        self.config
            .as_ref()
            .and_then(|config| config.play())
            .is_some_and(|source| source.enabled)
    }

    pub(crate) fn reply(&mut self, reply: Reply) {
        match reply {
            Reply::Note(message) => self.say(clean(&message)),
            Reply::Transfer(done, total) => self.transfer = Some((done, total)),
            Reply::Local { installed, config } => {
                self.installed = installed;
                match config {
                    Ok(config) => self.config = Some(config),
                    Err(error) => self.complain(clean(&error)),
                }
                if self.view != View::Search {
                    let row = self.selected();
                    self.select(row);
                }
            }
            Reply::Apps(Ok((apps, notes))) => {
                self.apps = Some(apps);
                self.notes = notes.iter().map(|note| clean(note)).collect();
                self.search();
            }
            Reply::Found(Ok((found, notes))) => {
                // Play's earlier results give way to these.
                let play = self.config.as_ref().and_then(|config| config.play());
                let mut apps = self.apps.take().unwrap_or_default();
                if let Some(play) = play {
                    apps.retain(|app| app.source != play.name);
                }
                apps.extend(found);
                self.apps = Some(apps);
                self.notes.extend(notes.iter().map(|note| clean(note)));
                self.search();
            }
            Reply::Found(Err(_)) => {}
            Reply::Permissions(permissions) => self.permissions = Some(permissions),
            Reply::Apps(Err(error)) => {
                self.apps = Some(Vec::new());
                self.notes = vec![clean(&error)];
                self.search();
            }
            Reply::Done(result) => self.done(result),
            Reply::Failed(message) => {
                self.failed = Some(message);
                self.quit = true;
            }
        }
    }

    fn done(&mut self, result: Result<String, String>) {
        self.busy = None;
        self.transfer = None;
        // A form waiting for this answer closes on success and shows the error otherwise.
        if let Some(Modal::Form(mut form)) = self.modal.take() {
            if form.pending {
                form.pending = false;
                match &result {
                    Ok(_) => {}
                    Err(error) => {
                        form.error = Some(clean(error));
                        self.modal = Some(Modal::Form(form));
                    }
                }
            } else {
                self.modal = Some(Modal::Form(form));
            }
        }
        match result {
            Ok(report) if report.is_empty() => {
                if let Some(note) = self.notes.first().cloned() {
                    self.complain(note);
                } else if let Some(apps) = &self.apps {
                    let text = format!("{} apps in the enabled sources", apps.len());
                    self.say(text);
                }
            }
            Ok(report) => self.say(summary(&report)),
            Err(error) => {
                let text = format!("error: {}", summary(&error));
                self.complain(text);
            }
        }
        if self.quit_pending {
            self.quit = true;
        }
    }
}
