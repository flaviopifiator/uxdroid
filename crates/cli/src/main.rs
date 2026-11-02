use std::ffi::OsString;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::ExitCode;

use uxdroid_apk::{Apk, host_abi, inspect};

const USAGE: &str = "usage: uxdroid inspect <apk>";
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    // `args_os`: paths on Linux are bytes and `args` panics on non-UTF-8 ones.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [command, apk] = args.as_slice() else {
        return fail(USAGE, EXIT_USAGE);
    };
    if command != "inspect" {
        return fail(USAGE, EXIT_USAGE);
    }

    let report = match inspect(Path::new(apk)) {
        Ok(apk) => render(&apk, host_abi()),
        Err(error) => return fail(&format!("error: {error}"), EXIT_FAILURE),
    };
    match std::io::stdout().write_all(report.as_bytes()) {
        // A closed pipe, as in `uxdroid inspect app.apk | head -1`, is not a failure.
        Err(error) if error.kind() != ErrorKind::BrokenPipe => fail(
            &format!("error: cannot write output: {error}"),
            EXIT_FAILURE,
        ),
        _ => ExitCode::SUCCESS,
    }
}

/// Reports on stderr, keeping the exit code even if stderr is closed; `eprintln!` would panic.
fn fail(message: &str, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{message}");
    ExitCode::from(code)
}

fn render(apk: &Apk, host_abi: &str) -> String {
    let manifest = &apk.manifest;
    // APK strings are untrusted; escaping stops them from forging lines of the report.
    let safe = |text: &str| text.escape_debug().to_string();
    let known = |value: &Option<String>| value.as_deref().map_or("unknown".to_string(), safe);
    let abis = if apk.abis.is_empty() {
        "none".to_string()
    } else {
        let counts: Vec<String> = apk
            .abis
            .iter()
            .map(|(abi, count)| format!("{} ({count})", safe(abi)))
            .collect();
        counts.join(", ")
    };

    format!(
        "package: {}\n\
         version: {} ({})\n\
         sdk: min {}, target {}\n\
         launcher: {}\n\
         dex: {} files, {} classes, {} methods\n\
         abis: {abis}\n\
         execution: {}\n",
        safe(&manifest.package),
        known(&manifest.version_name),
        known(&manifest.version_code),
        known(&manifest.min_sdk),
        known(&manifest.target_sdk),
        manifest
            .launcher
            .as_deref()
            .map_or("none".to_string(), safe),
        apk.dex.files,
        apk.dex.classes,
        apk.dex.methods,
        safe(&apk.execution_path(host_abi).to_string()),
    )
}
