use std::ffi::OsString;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::ExitCode;

use uxdroid_apk::{Apk, host_abi, inspect};

const INSPECT_USAGE: &str = "usage: uxdroid inspect <apk>";
const LOAD_USAGE: &str = "usage: uxdroid load <lib.so> [--search-dir <dir>]... [--call <symbol>]";
const RUN_USAGE: &str = "usage: uxdroid run <apk> [--runtime <dir>] [--exit-after-first-frame]";
const INSTALL_USAGE: &str = "usage: uxdroid install <apk>... --data <dir>";
const USAGE: &str = "usage: uxdroid inspect <apk>\n       \
                     uxdroid load <lib.so> [--search-dir <dir>]... [--call <symbol>]\n       \
                     uxdroid run <apk> [--runtime <dir>] [--exit-after-first-frame]\n       \
                     uxdroid install <apk>... --data <dir>\n       \
                     uxdroid service enable|disable|status|run <package> [<options>]\n       \
                     uxdroid mcp\n       \
                     uxdroid app <package> status|start|intent|stop [<options>]\n       \
                     uxdroid app <package> ui|screenshot|tap|type|scroll|back|key|wait [<options>]\n       \
                     uxdroid app list";
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    // `args_os`: paths on Linux are bytes and `args` panics on non-UTF-8 ones.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "inspect" => run_inspect(rest),
        Some((command, rest)) if command == "load" => native::run_load(rest),
        Some((command, rest)) if command == "run" => run_command(rest),
        Some((command, rest)) if command == "install" => install::run_install(rest),
        Some((command, rest)) if command == "app" => app_command(rest),
        Some((command, rest)) if command == "service" => service_command(rest),
        Some((command, rest)) if command == "mcp" => mcp_command(rest),
        _ => fail(USAGE, EXIT_USAGE),
    }
}

#[cfg(target_os = "linux")]
mod app;
mod install;
#[cfg(target_os = "linux")]
mod mcp;
mod runtime;
#[cfg(target_os = "linux")]
mod service;

#[cfg(target_os = "linux")]
fn mcp_command(args: &[OsString]) -> ExitCode {
    if !args.is_empty() {
        return fail("usage: uxdroid mcp", EXIT_USAGE);
    }
    mcp::run_mcp()
}

#[cfg(not(target_os = "linux"))]
fn mcp_command(_args: &[OsString]) -> ExitCode {
    fail("error: uxdroid mcp needs a Linux host", EXIT_FAILURE)
}

#[cfg(target_os = "linux")]
fn app_command(args: &[OsString]) -> ExitCode {
    app::run_app(args)
}

#[cfg(target_os = "linux")]
fn service_command(args: &[OsString]) -> ExitCode {
    service::run_service(args)
}

#[cfg(not(target_os = "linux"))]
fn service_command(_args: &[OsString]) -> ExitCode {
    fail("error: uxdroid service needs a Linux host", EXIT_FAILURE)
}

#[cfg(not(target_os = "linux"))]
fn app_command(_args: &[OsString]) -> ExitCode {
    fail("error: uxdroid app needs a Linux host", EXIT_FAILURE)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
mod run;

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn run_command(args: &[OsString]) -> ExitCode {
    run::run_app(args)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn run_command(_args: &[OsString]) -> ExitCode {
    fail(
        "error: uxdroid run needs an x86_64 glibc host",
        EXIT_FAILURE,
    )
}

fn run_inspect(args: &[OsString]) -> ExitCode {
    let [apk] = args else {
        return fail(INSPECT_USAGE, EXIT_USAGE);
    };
    match inspect(Path::new(apk)) {
        Ok(apk) => print(&render(&apk, host_abi())),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

fn print(report: &str) -> ExitCode {
    let mut stdout = std::io::stdout();
    match stdout
        .write_all(report.as_bytes())
        .and_then(|()| stdout.flush())
    {
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

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
mod native {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use uxdroid_bridge::{Library, load};

    use crate::{EXIT_FAILURE, EXIT_USAGE, LOAD_USAGE, fail, print};

    struct LoadArgs<'a> {
        library: &'a Path,
        search_dirs: Vec<PathBuf>,
        call: Option<&'a str>,
    }

    fn parse(args: &[OsString]) -> Option<LoadArgs<'_>> {
        let (library, mut rest) = args.split_first()?;
        if library.as_encoded_bytes().starts_with(b"--") {
            return None;
        }
        let mut parsed = LoadArgs {
            library: Path::new(library),
            search_dirs: Vec::new(),
            call: None,
        };
        while let Some((flag, values)) = rest.split_first() {
            let (value, remaining) = values.split_first()?;
            if flag == "--search-dir" {
                parsed.search_dirs.push(PathBuf::from(value));
            } else if flag == "--call" && parsed.call.is_none() {
                parsed.call = Some(value.to_str()?);
            } else {
                return None;
            }
            rest = remaining;
        }
        Some(parsed)
    }

    fn describe(library: &Library) -> String {
        let name = library.name().escape_debug();
        let needed: Vec<String> = library
            .needed()
            .iter()
            .map(|name| name.escape_debug().to_string())
            .collect();
        format!("loaded: {name}\nneeds: {}\n", needed.join(", "))
    }

    pub fn run_load(args: &[OsString]) -> ExitCode {
        let Some(args) = parse(args) else {
            return fail(LOAD_USAGE, EXIT_USAGE);
        };
        let library = match load(args.library, &args.search_dirs) {
            Ok(library) => library,
            Err(error) => return fail(&format!("error: {error}"), EXIT_FAILURE),
        };
        // Printed before the call: the library's own output, or its crash, comes after this.
        let code = print(&describe(library));
        let Some(symbol) = args.call else {
            return code;
        };
        // SAFETY: `--call` documents that the symbol must be `int symbol(void)`.
        match unsafe { library.call_i32(symbol) } {
            Some(result) => print(&format!("call {}: {result}\n", symbol.escape_debug())),
            None => fail(
                &format!(
                    "error: {} has no symbol {}",
                    library.name().escape_debug(),
                    symbol.escape_debug()
                ),
                EXIT_FAILURE,
            ),
        }
    }
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
mod native {
    use std::ffi::OsString;
    use std::process::ExitCode;

    use crate::{EXIT_FAILURE, fail};

    pub fn run_load(_args: &[OsString]) -> ExitCode {
        fail(
            "error: uxdroid load needs an x86_64 glibc host",
            EXIT_FAILURE,
        )
    }
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
