//! Where uxdroid's own messages and the app's log go: the stderr the process started with.
//!
//! On Android the log is a daemon, apart from the process's file descriptors, and an app may point
//! its own stdout and stderr anywhere. Some do: AnkiDroid's Rust backend `dup2`s a pipe over both and
//! writes every line it reads back to the log. With the log on file descriptor 2 that is a loop, and
//! every later line, uxdroid's included, went into the app's pipe. So each library that writes here
//! keeps a duplicate of descriptor 2 from the moment it loads, before any app code runs, and writes to
//! that, whatever the app does with descriptor 2 later.

use std::sync::atomic::{AtomicI32, Ordering};

/// The kept duplicate of the process's stderr, or -1 if none could be made.
static STDERR: AtomicI32 = AtomicI32::new(-1);

/// Lowest descriptor the duplicate may take, so it never lands on 0, 1 or 2.
const FIRST_FREE: libc::c_int = 3;

extern "C" fn keep_stderr() {
    // SAFETY: fcntl on descriptor 2 only reads it; a closed one makes it fail with -1.
    let fd = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, FIRST_FREE) };
    STDERR.store(fd, Ordering::Relaxed);
}

// Runs when the library loads, before ART can load any of the app's code.
#[used]
#[unsafe(link_section = ".init_array")]
static KEEP_STDERR: extern "C" fn() = keep_stderr;

/// The descriptor messages go to: the kept stderr, or descriptor 2 when none was kept.
pub fn stderr_fd() -> libc::c_int {
    // A library links this crate as an rlib, whose objects the linker takes only when something in
    // them is referenced; naming the initializer here keeps it wherever messages are written.
    std::hint::black_box(&KEEP_STDERR);
    match STDERR.load(Ordering::Relaxed) {
        -1 => libc::STDERR_FILENO,
        fd => fd,
    }
}

/// Writes all of `bytes` to `fd` in as few writes as the kernel takes, so a line from one thread
/// does not land inside another's. Errors are dropped, as a log has nowhere to report them.
pub fn write_all(fd: libc::c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: the pointer and length describe `bytes`.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

/// Writes a message to the kept stderr in one piece.
pub fn write_stderr(message: std::fmt::Arguments<'_>) {
    match message.as_str() {
        Some(text) => write_all(stderr_fd(), text.as_bytes()),
        None => write_all(stderr_fd(), message.to_string().as_bytes()),
    }
}

/// `eprintln!` to the kept stderr: one line, written whole.
#[macro_export]
macro_rules! report {
    ($($argument:tt)*) => {
        $crate::output::write_stderr(format_args!("{}\n", format_args!($($argument)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_kept_a_descriptor_apart_from_stderr_when_it_loaded() {
        let fd = stderr_fd();
        assert!(fd >= FIRST_FREE, "{fd}");
        // SAFETY: F_GETFD only reads the descriptor's flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(
            flags & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "programs the app runs must not inherit it"
        );
    }

    #[test]
    fn a_message_reaches_its_descriptor_whole() {
        let mut fds = [0; 2];
        // SAFETY: pipe fills the two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        write_all(fds[1], format!("{}\n", "x".repeat(10_000)).as_bytes());
        let mut read = vec![0u8; 20_000];
        let mut total = 0;
        while total < 10_001 {
            // SAFETY: reads into the unused part of `read`.
            let n = unsafe {
                libc::read(
                    fds[0],
                    read[total..].as_mut_ptr().cast(),
                    read.len() - total,
                )
            };
            assert!(n > 0);
            total += n as usize;
        }
        assert_eq!(total, 10_001);
        // SAFETY: both descriptors are this test's.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
