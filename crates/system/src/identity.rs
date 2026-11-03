//! The app's Android identity, which is not its Linux one.
//!
//! Android runs each installed app under a uid of its own, from `FIRST_APPLICATION_UID` (10000) to
//! `LAST_APPLICATION_UID` (19999), and its framework reads uids below that as the system's: uid 1000
//! is `SYSTEM_UID`, which WebView refuses ("not allowed in privileged processes") and TrafficStats
//! cannot initialize for. Here the process keeps the user's Linux uid, which owns its files, and the
//! framework sees the app's own uid: `Binder.getCallingUid`, which `Process.myUid` is rewritten to
//! call (`runtime/framework/rewrite_classes.py`), `ApplicationInfo.uid` and the services in process.
//! Nothing crosses to another process, so the uid only needs to be stable for a package.

use std::sync::atomic::{AtomicI32, Ordering};

/// Android's `Process.FIRST_APPLICATION_UID`.
pub const FIRST_APPLICATION_UID: u32 = 10_000;
/// How many application uids Android has, up to `Process.LAST_APPLICATION_UID`.
const APPLICATION_UIDS: u32 = 10_000;

/// The uid the framework sees, or -1 before an app is started in this process.
static APPLICATION: AtomicI32 = AtomicI32::new(-1);

/// The uid of an installed package: the same for a package name on every install and host, so files
/// and settings that record it stay valid. FNV-1a of the name picks one of Android's 10,000
/// application uids; two packages can share one, which only matters between processes.
pub fn application_uid(package: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in package.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    FIRST_APPLICATION_UID + hash % APPLICATION_UIDS
}

/// From now on, the framework sees this process as `package`. Returns its uid.
pub fn become_application(package: &str) -> u32 {
    let uid = application_uid(package);
    APPLICATION.store(uid as i32, Ordering::Relaxed);
    uid
}

/// The uid the framework sees: the app's, once one is started, else the Linux uid, as for programs
/// such as the installer that run the framework without an app.
pub fn uid() -> i32 {
    match APPLICATION.load(Ordering::Relaxed) {
        // SAFETY: getuid cannot fail.
        -1 => unsafe { libc::getuid() as i32 },
        uid => uid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_gets_an_application_uid_that_never_changes() {
        // Pinned: a different value would orphan what an earlier install recorded.
        assert_eq!(application_uid("com.example.compat"), 14_194);
        assert_eq!(
            application_uid(""),
            FIRST_APPLICATION_UID + 0x811c_9dc5 % 10_000
        );
        for package in [
            "com.example.compat",
            "org.isoron.uhabits",
            "com.ichi2.anki",
            "a",
        ] {
            let uid = application_uid(package);
            assert!((10_000..=19_999).contains(&uid), "{package}: {uid}");
        }
        assert_ne!(
            application_uid("org.isoron.uhabits"),
            application_uid("net.gsantner.markor")
        );
    }

    #[test]
    fn the_framework_sees_the_linux_uid_until_an_app_starts_then_the_apps() {
        // SAFETY: getuid cannot fail.
        let linux = unsafe { libc::getuid() } as i32;
        assert_eq!(uid(), linux);
        let uid_of_app = become_application("com.example.compat");
        assert_eq!(uid(), uid_of_app as i32);
        assert_ne!(uid(), 1000, "the system uid");
        APPLICATION.store(-1, Ordering::Relaxed);
    }
}
