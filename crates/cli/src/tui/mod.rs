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
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
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
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

fn run(data: PathBuf) -> std::io::Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    let (jobs, replies) = worker::spawn(data.clone(), worker::http(), cancel.clone());
    let mut state = State::new(data, cancel);
    // A panic message printed on the alternate screen would vanish with it, so the terminal is
    // restored before the message is printed.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    let _session = Session::start(std::io::stdout(), true)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    event_loop(&mut terminal, &mut state, &jobs, &replies, &mut |timeout| {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    })
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

fn restore() {
    let _ = leave(&mut std::io::stdout());
    let _ = disable_raw_mode();
}

/// Draws, reads one event or waits up to 50 ms for one, takes the worker's replies, and sends it
/// the jobs they cause, until the user quits. Draws only after something changed.
fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    jobs: &Sender<Job>,
    replies: &Receiver<Reply>,
    next_event: &mut dyn FnMut(Duration) -> std::io::Result<Option<Event>>,
) -> std::io::Result<()> {
    let mut changed = true;
    loop {
        for job in state.take_jobs() {
            if jobs.send(job).is_err() {
                return Err(std::io::Error::other("the store worker stopped"));
            }
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
        while let Ok(reply) = replies.try_recv() {
            state.reply(reply);
            changed = true;
        }
    }
}
