//! `uxdroid tui`: a terminal UI over the store commands. It searches the cached indexes as the user
//! types, and installs, updates, removes and changes sources through the same functions as
//! `uxdroid search`, `install`, `update`, `remove`, `list` and `sources`, run on a worker thread.

mod state;
#[cfg(test)]
mod tests;
mod view;
mod worker;

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

use state::State;
use worker::{Job, Reply};

use crate::store::Args;
use crate::{EXIT_FAILURE, EXIT_USAGE, fail};

pub(crate) const TUI_USAGE: &str = "usage: uxdroid tui [--data <dir>]";

pub(crate) fn run_tui(args: &[OsString]) -> ExitCode {
    let Some(parsed) =
        Args::parse(args, &[("--data", true)]).filter(|parsed| parsed.words.is_empty())
    else {
        return fail(TUI_USAGE, EXIT_USAGE);
    };
    let data = match parsed.data() {
        Ok(data) => data,
        Err(error) => return fail(&format!("error: {error}"), EXIT_FAILURE),
    };
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return fail(
            "error: uxdroid tui needs a terminal; uxdroid search, install, update, remove, list and sources work without one",
            EXIT_FAILURE,
        );
    }
    match run(data) {
        Ok(None) => ExitCode::SUCCESS,
        // Ended by a signal: with the terminal restored, the process ends by it as it would have,
        // so a shell or a supervisor sees the signal.
        Ok(Some(signal)) => {
            let _ = signal_hook::low_level::emulate_default_handler(signal);
            ExitCode::from(128u8.saturating_add(signal as u8))
        }
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

/// The signals that end the UI as quitting does: a kill, Ctrl-C sent from elsewhere (raw mode
/// keeps the terminal from sending it), and the terminal closing.
const SIGNALS: [i32; 3] = [
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGHUP,
];

/// Runs the UI until it quits, and returns the signal that ended it, if one did.
fn run(data: PathBuf) -> std::io::Result<Option<i32>> {
    let caught = Arc::new(AtomicUsize::new(0));
    for signal in SIGNALS {
        signal_hook::flag::register_usize(signal, caught.clone(), signal as usize)?;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let (jobs, replies) = worker::spawn(data.clone(), worker::http(), cancel.clone());
    let mut state = State::new(data, cancel);
    // A panic message printed on the alternate screen would vanish with it, so the terminal is
    // restored before the message is printed. The worker's panic reaches the UI as its last reply
    // instead, and the UI restores the terminal as it ends.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if restores_on_panic(std::thread::current().name()) {
            restore();
            previous(info);
        }
    }));
    let result = {
        let _session = Session::start(std::io::stdout(), true)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        event_loop(
            &mut terminal,
            &mut state,
            &jobs,
            &replies,
            &caught,
            &mut |timeout| {
                if event::poll(timeout)? {
                    event::read().map(Some)
                } else {
                    Ok(None)
                }
            },
        )
    };
    // A signal ends the UI even when the terminal it hung up on can no longer be drawn on.
    match (state.signalled, result) {
        (Some(signal), _) => Ok(Some(signal)),
        (None, result) => result.map(|()| None),
    }
}

/// The terminal in the UI's mode: raw input, the alternate screen and a hidden cursor. Dropping it,
/// on return, error or panic, puts the terminal back as it was.
pub(crate) struct Session<W: Write> {
    out: W,
    raw: bool,
}

impl<W: Write> Session<W> {
    /// Enters the UI's mode on `out`; `raw` also switches the controlling terminal to raw input.
    pub(crate) fn start(out: W, raw: bool) -> std::io::Result<Self> {
        if raw {
            enable_raw_mode()?;
        }
        let mut session = Self { out, raw };
        execute!(session.out, EnterAlternateScreen, Hide)?;
        Ok(session)
    }
}

impl<W: Write> Drop for Session<W> {
    fn drop(&mut self) {
        let _ = leave(&mut self.out);
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}

fn leave(out: &mut impl Write) -> std::io::Result<()> {
    execute!(out, Show, LeaveAlternateScreen)
}

/// Whether a panic on `thread` restores the terminal at once. The worker's does not: the UI still
/// draws on the alternate screen, learns of it from the worker's last reply and ends itself.
fn restores_on_panic(thread: Option<&str>) -> bool {
    thread != Some(worker::NAME)
}

fn restore() {
    let _ = leave(&mut std::io::stdout());
    let _ = disable_raw_mode();
}

/// Why the worker is gone: its panic, as its last reply says, or only that it stopped.
fn stopped(state: &mut State, replies: &Receiver<Reply>) -> std::io::Error {
    while let Ok(reply) = replies.try_recv() {
        state.reply(reply);
    }
    match &state.failed {
        Some(message) => std::io::Error::other(format!("the store worker failed: {message}")),
        None => std::io::Error::other("the store worker stopped"),
    }
}

/// Draws, reads one event or waits up to 50 ms for one, takes the worker's replies and the signals
/// caught (a signal number, 0 for none), and sends the worker the jobs they cause, until the user
/// quits. Draws only after something changed. A worker that panicked or stopped ends it with an
/// error.
fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    jobs: &Sender<Job>,
    replies: &Receiver<Reply>,
    signals: &AtomicUsize,
    next_event: &mut dyn FnMut(Duration) -> std::io::Result<Option<Event>>,
) -> std::io::Result<()> {
    let mut changed = true;
    loop {
        for job in state.take_jobs() {
            if jobs.send(job).is_err() {
                return Err(stopped(state, replies));
            }
        }
        if state.failed.is_some() {
            return Err(stopped(state, replies));
        }
        if changed {
            terminal
                .draw(|frame| view::draw(frame, state))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            changed = false;
        }
        if state.quit {
            return Ok(());
        }
        if let Some(event) = next_event(Duration::from_millis(50))? {
            state.event(event);
            changed = true;
        }
        let signal = signals.swap(0, Ordering::SeqCst);
        if signal != 0 {
            state.signal(signal as i32);
            changed = true;
        }
        loop {
            match replies.try_recv() {
                Ok(reply) => {
                    state.reply(reply);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(stopped(state, replies)),
            }
        }
    }
}
