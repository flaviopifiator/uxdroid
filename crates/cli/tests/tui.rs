//! `uxdroid tui` in a pseudo-terminal it has as its controlling terminal, as a terminal emulator
//! runs it: signals end it with the terminal's modes, screen and cursor as they were.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("tui-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// A pseudo-terminal of 80x24: its controller and the path of its terminal.
fn pty() -> (OwnedFd, String) {
    // SAFETY: plain libc calls on a descriptor this function owns; the name is copied out at once.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0);
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(libc::ioctl(master, libc::TIOCSWINSZ, &size), 0);
        let name = std::ffi::CStr::from_ptr(name.as_ptr())
            .to_string_lossy()
            .into_owned();
        (OwnedFd::from_raw_fd(master), name)
    }
}

/// The terminal's modes, as `stty -g` would print them.
fn modes(terminal: &std::fs::File) -> Vec<u32> {
    // SAFETY: tcgetattr fills the struct it is given.
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut termios) },
        0
    );
    let mut modes = vec![
        termios.c_iflag,
        termios.c_oflag,
        termios.c_cflag,
        termios.c_lflag,
    ];
    modes.extend(termios.c_cc.iter().map(|c| u32::from(*c)));
    modes
}

fn screen(output: &Mutex<Vec<u8>>) -> String {
    String::from_utf8_lossy(&output.lock().unwrap()).into_owned()
}

#[test]
fn a_signal_to_the_terminal_ui_restores_the_terminal_and_ends_it_by_that_signal() {
    let data = scratch("signals").join("data");
    let status = Command::new(env!("CARGO_BIN_EXE_uxdroid"))
        .args(["sources", "disable", "fdroid", "--data"])
        .arg(&data)
        .output()
        .unwrap();
    assert!(status.status.success());
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let (master, name) = pty();
        let terminal = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        let before = modes(&terminal);
        let mut command = Command::new(env!("CARGO_BIN_EXE_uxdroid"));
        command
            .args(["tui", "--data"])
            .arg(&data)
            .env("TERM", "xterm-256color")
            .env("UXDROID_RUNTIME", "")
            .stdin(Stdio::from(terminal.try_clone().unwrap()))
            .stdout(Stdio::from(terminal.try_clone().unwrap()))
            .stderr(Stdio::from(terminal.try_clone().unwrap()));
        // SAFETY: setsid and TIOCSCTTY only, between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        // The command holds copies of the terminal; the reader sees its end only without them.
        drop(command);
        let output = Arc::new(Mutex::new(Vec::new()));
        let reader = {
            let output = output.clone();
            let mut master = std::fs::File::from(master.try_clone().unwrap());
            std::thread::spawn(move || {
                let mut buffer = [0; 4096];
                while let Ok(read) = master.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    output.lock().unwrap().extend_from_slice(&buffer[..read]);
                }
            })
        };
        let started = Instant::now();
        // Idle: its first read found no enabled source.
        while !screen(&output).contains("no source is enabled") {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{}",
                screen(&output)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(modes(&terminal), before, "raw while it runs");
        // SAFETY: the child's own pid.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "it ends");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.signal(), Some(signal), "{status:?}");
        assert_eq!(modes(&terminal), before, "signal {signal}: modes restored");
        std::thread::sleep(Duration::from_millis(100));
        let text = screen(&output);
        let entered = text.rfind("\u{1b}[?1049h").expect("the alternate screen");
        let rest = &text[entered..];
        assert!(
            rest.contains("\u{1b}[?1049l"),
            "signal {signal}: left the alternate screen"
        );
        assert!(
            rest.contains("\u{1b}[?25h"),
            "signal {signal}: cursor shown"
        );
        drop(terminal);
        drop(master);
        let _ = reader.join();
    }
}
