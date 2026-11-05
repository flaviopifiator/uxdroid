//! Measures how much of each Android NDK system library uxdroid binds, without running an app.
//!
//! The official export lists are the NDK's stub libraries for the highest API level it ships,
//! which the NDK generates from AOSP's `.map.txt` files. Each exported symbol is classified as the
//! x86_64 bridge would bind it for an x86_64 library, and as the arm64 planner would for an
//! arm64-v8a library that needs it. See `docs/coverage.md`.
//!
//! Usage: `cargo run -p uxdroid-cli --example coverage -- [NDK root] [TSV output]`

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let ndk = args
        .next()
        .unwrap_or_else(|| "/opt/android-ndk".to_string());
    let tsv = args.next();
    match audit::run(std::path::Path::new(&ndk), tsv.as_deref()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coverage: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")))]
fn main() {
    eprintln!("coverage: only an x86_64 glibc host has both the bridge and the arm64 translator");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
mod audit {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use uxdroid_bridge::guest::plan::{self, Binding, GuestLibrary, Symbol};
    use uxdroid_bridge::table::Table;
    use uxdroid_bridge::{HostBinding, host_binding};

    const PREBUILT: &str = "toolchains/llvm/prebuilt/linux-x86_64";
    /// Apps ship their own C++ runtime; the system one is not part of the NDK's stable ABI.
    const SKIPPED: &[&str] = &["libc++.so"];

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Engine {
        Bridge,
        Translator,
    }

    impl Engine {
        fn label(self) -> &'static str {
            match self {
                Engine::Bridge => "x86_64-bridge",
                Engine::Translator => "arm64-translator",
            }
        }

        fn triple(self) -> &'static str {
            match self {
                Engine::Bridge => "x86_64-linux-android",
                Engine::Translator => "aarch64-linux-android",
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Status {
        Resolved,
        Trap,
        Unimplemented,
        Missing,
    }

    impl Status {
        fn label(self) -> &'static str {
            match self {
                Status::Resolved => "resolved",
                Status::Trap => "trap",
                Status::Unimplemented => "unimplemented",
                Status::Missing => "missing",
            }
        }
    }

    struct Export {
        name: String,
        is_data: bool,
    }

    struct Row {
        name: String,
        is_data: bool,
        status: Status,
        detail: String,
    }

    pub fn run(ndk: &Path, tsv: Option<&str>) -> Result<(), String> {
        let nm = ndk.join(PREBUILT).join("bin/llvm-nm");
        let tables: Vec<&'static Table> = uxdroid_ndk::tables().into_iter().collect();
        let mut report = String::new();
        let mut lines = String::from("path\tapi\tlibrary\tsymbol\tkind\tstatus\tdetail\n");
        for path in [Engine::Bridge, Engine::Translator] {
            let base = ndk
                .join(PREBUILT)
                .join("sysroot/usr/lib")
                .join(path.triple());
            let (api, directory) = highest_api(&base)?;
            let mut summary = String::new();
            let mut missing_lists = String::new();
            let _ = writeln!(
                summary,
                "\n{} (NDK stubs, API {api}, {})\n{:<20} {:>6} {:>6} {:>6} {:>9} {:>5} {:>7} {:>8} {:>9}",
                path.label(),
                directory.display(),
                "library",
                "total",
                "funcs",
                "data",
                "resolved",
                "trap",
                "unimpl",
                "missing",
                "resolved%"
            );
            let mut totals = [0usize; 7];
            for library in libraries(&directory)? {
                let exports = exports(&nm, &directory.join(&library))?;
                let rows = classify(path, &library, &exports, &tables);
                let count = |status| rows.iter().filter(|row| row.status == status).count();
                let data = rows.iter().filter(|row| row.is_data).count();
                let counts = [
                    rows.len(),
                    rows.len() - data,
                    data,
                    count(Status::Resolved),
                    count(Status::Trap),
                    count(Status::Unimplemented),
                    count(Status::Missing),
                ];
                for (total, value) in totals.iter_mut().zip(counts) {
                    *total += value;
                }
                summary.push_str(&summary_line(&library, counts));
                let missing: Vec<String> = rows
                    .iter()
                    .filter(|row| row.status == Status::Missing)
                    .map(|row| match row.is_data {
                        true => format!("{} (data)", row.name),
                        false => row.name.clone(),
                    })
                    .collect();
                if !missing.is_empty() {
                    let _ = writeln!(
                        missing_lists,
                        "{} {library} missing ({}): {}",
                        path.label(),
                        missing.len(),
                        missing.join(" ")
                    );
                }
                for row in &rows {
                    let _ = writeln!(
                        lines,
                        "{}\t{api}\t{library}\t{}\t{}\t{}\t{}",
                        path.label(),
                        row.name,
                        if row.is_data { "data" } else { "function" },
                        row.status.label(),
                        row.detail
                    );
                }
            }
            summary.push_str(&summary_line("TOTAL", totals));
            report.push_str(&summary);
            report.push('\n');
            report.push_str(&missing_lists);
        }
        print!("{report}");
        if let Some(tsv) = tsv {
            std::fs::write(tsv, lines).map_err(|error| format!("{tsv}: {error}"))?;
        }
        Ok(())
    }

    fn summary_line(
        library: &str,
        [total, funcs, data, resolved, trap, unimplemented, missing]: [usize; 7],
    ) -> String {
        let percent = match total {
            0 => 100.0,
            total => 100.0 * resolved as f64 / total as f64,
        };
        format!(
            "{library:<20} {total:>6} {funcs:>6} {data:>6} {resolved:>9} {trap:>5} {unimplemented:>7} {missing:>8} {percent:>8.1}%\n"
        )
    }

    fn highest_api(base: &Path) -> Result<(u32, PathBuf), String> {
        std::fs::read_dir(base)
            .map_err(|error| format!("{}: {error}", base.display()))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let api = entry.file_name().to_str()?.parse::<u32>().ok()?;
                Some((api, entry.path()))
            })
            .max_by_key(|(api, _)| *api)
            .ok_or_else(|| format!("{}: no API level directory", base.display()))
    }

    fn libraries(directory: &Path) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with("lib") && name.ends_with(".so"))
            .filter(|name| !SKIPPED.contains(&name.as_str()))
            .collect();
        names.sort();
        Ok(names)
    }

    /// The defined dynamic symbols of a stub library, each name once, from `llvm-nm`.
    fn exports(nm: &Path, library: &Path) -> Result<Vec<Export>, String> {
        let output = Command::new(nm)
            .args(["-D", "--defined-only"])
            .arg(library)
            .output()
            .map_err(|error| format!("{}: {error}", nm.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} {}: {}",
                nm.display(),
                library.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let mut exports: Vec<Export> = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split_whitespace();
            let (Some(_address), Some(kind), Some(name)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let is_data = match kind {
                "T" | "t" | "W" | "w" | "i" => false,
                "D" | "d" | "B" | "b" | "R" | "r" | "V" | "v" | "u" => true,
                // Absolute symbols are version names such as LIBC, not exports.
                _ => continue,
            };
            // Versioned names: `name@@LIBC` and `name@LIBC_N` are one export.
            let name = name.split('@').next().unwrap_or(name);
            if name.is_empty() || exports.iter().any(|known| known.name == name) {
                continue;
            }
            exports.push(Export {
                name: name.to_string(),
                is_data,
            });
        }
        exports.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(exports)
    }

    fn classify(
        path: Engine,
        library: &str,
        exports: &[Export],
        tables: &[&'static Table],
    ) -> Vec<Row> {
        if !plan::provided(library, tables) {
            return exports
                .iter()
                .map(|export| Row {
                    name: export.name.clone(),
                    is_data: export.is_data,
                    status: Status::Missing,
                    detail: "the library is not provided".to_string(),
                })
                .collect();
        }
        match path {
            Engine::Bridge => {
                let table: Vec<&'static Table> = tables
                    .iter()
                    .copied()
                    .filter(|table| table.library == library)
                    .collect();
                exports
                    .iter()
                    .map(|export| {
                        let binding =
                            host_binding(&export.name, &table, &[library], export.is_data);
                        let (status, detail) = match binding {
                            HostBinding::Table(library) => {
                                (Status::Resolved, format!("table {library}"))
                            }
                            HostBinding::Shim => (Status::Resolved, "shim".to_string()),
                            HostBinding::Glibc => (Status::Resolved, "audited glibc".to_string()),
                            HostBinding::Trap(why) => (Status::Trap, why.to_string()),
                            HostBinding::Unimplemented(library) => (
                                Status::Unimplemented,
                                format!("a stub naming it, of {library}"),
                            ),
                            HostBinding::Missing(why) => (Status::Missing, why.to_string()),
                        };
                        Row {
                            name: export.name.clone(),
                            is_data: export.is_data,
                            status,
                            detail,
                        }
                    })
                    .collect()
            }
            Engine::Translator => {
                // A guest library of no app that needs only this one, so nothing binds to the app.
                let probe = GuestLibrary {
                    name: "libuxdroid-coverage-probe.so".to_string(),
                    needed: vec![library.to_string()],
                    symbols: exports
                        .iter()
                        .map(|export| Symbol {
                            name: export.name.clone(),
                            defined: false,
                            weak: false,
                            function: !export.is_data,
                            object: export.is_data,
                            value: 0,
                            size: 0,
                        })
                        .collect(),
                };
                let imports = plan::plan(&[probe], tables).remove(0).imports;
                exports
                    .iter()
                    .zip(imports)
                    .map(|(export, import)| {
                        let (status, detail) = match import.binding {
                            Binding::Table(library) => {
                                (Status::Resolved, format!("table {library}"))
                            }
                            Binding::Libc => (Status::Resolved, "guest libc".to_string()),
                            Binding::Stub(why) => (Status::Trap, why.to_string()),
                            Binding::Unimplemented(library) => (
                                Status::Unimplemented,
                                format!("a stub naming it, of {library}"),
                            ),
                            Binding::Unresolved => (Status::Missing, "unresolved".to_string()),
                            Binding::App(owner) => (Status::Resolved, format!("app {owner}")),
                        };
                        Row {
                            name: export.name.clone(),
                            is_data: export.is_data,
                            status,
                            detail,
                        }
                    })
                    .collect()
            }
        }
    }
}
