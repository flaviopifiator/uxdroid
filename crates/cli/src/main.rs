use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use uxdroid_apk::{Apk, host_abi, inspect};

const USAGE: &str = "usage: uxdroid inspect <apk>";
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, apk] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    };
    if command != "inspect" {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    }

    match inspect(Path::new(apk)) {
        Ok(apk) => {
            // A closed pipe, as in `uxdroid inspect app.apk | head -1`, is not a failure.
            let _ = std::io::stdout().write_all(render(&apk, host_abi()).as_bytes());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn render(apk: &Apk, host_abi: &str) -> String {
    let manifest = &apk.manifest;
    let known = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".to_string());
    let abis = if apk.abis.is_empty() {
        "none".to_string()
    } else {
        let counts: Vec<String> = apk
            .abis
            .iter()
            .map(|(abi, count)| format!("{abi} ({count})"))
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
        manifest.package,
        known(&manifest.version_name),
        known(&manifest.version_code),
        known(&manifest.min_sdk),
        known(&manifest.target_sdk),
        manifest.launcher.as_deref().unwrap_or("none"),
        apk.dex.files,
        apk.dex.classes,
        apk.dex.methods,
        apk.execution_path(host_abi),
    )
}
