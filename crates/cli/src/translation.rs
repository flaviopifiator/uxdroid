//! Install-time work for an app whose native code exists only for `arm64-v8a`, which runs
//! translated on this x86_64 host: every import of every library is resolved now, against the
//! app's libraries, the system library tables and the guest libc, and the result is written next
//! to the libraries with each one's JNI entry points and function boundaries.
//!
//! An import the host has but guest code cannot call yet is recorded as a stub, which stops the
//! program naming it only if it is called: apps import much they never call on a given path. So is
//! a function on the NDK's official list of a library the app needs that uxdroid does not have yet,
//! and the install warns, naming those per library. Any other import found nowhere would fail the
//! load, so it fails the install instead, naming it.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use uxdroid_bridge::guest::plan::{self, Binding, LibraryPlan};

pub(crate) const GUEST_ABI: &str = "arm64-v8a";
pub(crate) const PLAN_FILE: &str = "arm64-v8a.plan.json";
/// Stub names listed in the install's report; the plan has them all.
const LISTED_STUBS: usize = 20;

#[derive(Default)]
pub(crate) struct Summary {
    pub app: usize,
    pub tables: usize,
    pub libc: usize,
    pub weak_null: usize,
    pub stubs: Vec<String>,
    /// Each library with imports of NDK functions uxdroid does not have yet, and their names.
    pub unimplemented: Vec<(String, Vec<String>)>,
}

impl Summary {
    /// The install's report: one line, then a warning per library calling functions uxdroid
    /// does not have yet.
    pub(crate) fn describe(&self, plan: &Path, package: &str) -> String {
        let unimplemented: usize = self
            .unimplemented
            .iter()
            .map(|(_, names)| names.len())
            .sum();
        let total =
            self.app + self.tables + self.libc + self.weak_null + self.stubs.len() + unimplemented;
        let mut line = format!(
            "translated for x86_64: {total} imports resolved at install, {} from the app's libraries, {} from system library tables, {} from the guest libc, {} weak ones null, {} bound to stubs that stop the program naming them if called",
            self.app,
            self.tables,
            self.libc,
            self.weak_null,
            self.stubs.len() + unimplemented
        );
        if !self.stubs.is_empty() {
            let shown: Vec<String> = self
                .stubs
                .iter()
                .take(LISTED_STUBS)
                .map(|name| name.escape_debug().to_string())
                .collect();
            line.push_str(&format!(" ({}", shown.join(", ")));
            if self.stubs.len() > LISTED_STUBS {
                line.push_str(&format!(" and {} more", self.stubs.len() - LISTED_STUBS));
            }
            line.push(')');
        }
        line.push_str(&format!("; plan in {}\n", plan.display()));
        for (library, names) in &self.unimplemented {
            let names: Vec<String> = names
                .iter()
                .map(|name| name.escape_debug().to_string())
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            line.push_str(&format!(
                "warning: {}: {}\n",
                package.escape_debug(),
                uxdroid_bridge::unimplemented::warning(library, &names)
            ));
        }
        line
    }
}

fn binding_text(binding: &Binding, weak: bool) -> String {
    match binding {
        Binding::App(library) => format!("app {library}"),
        Binding::Table(library) => format!("table {library}"),
        Binding::Libc => "libc".to_string(),
        Binding::Stub(why) => format!("stub: {why}"),
        Binding::Unimplemented(library) => format!("unimplemented: {library}"),
        Binding::Unresolved if weak => "null (weak)".to_string(),
        Binding::Unresolved => "unresolved".to_string(),
    }
}

fn library_json(plan: &LibraryPlan) -> Value {
    let imports: BTreeMap<&str, String> = plan
        .imports
        .iter()
        .map(|import| {
            (
                import.name.as_str(),
                binding_text(&import.binding, import.weak),
            )
        })
        .collect();
    json!({
        "needed": plan.needed,
        "jni": plan.jni,
        "functions": plan.functions,
        "imports": imports,
    })
}

/// Plans the libraries extracted into `directory` and writes the plan there.
pub(crate) fn prepare(directory: &Path, package: &str) -> Result<Summary, String> {
    let cannot = |what: String| format!("cannot translate {}: {what}", package.escape_debug());
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map_err(|error| cannot(format!("{}: {error}", directory.display())))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".so"))
        .collect();
    names.sort();
    let mut libraries = Vec::with_capacity(names.len());
    for name in &names {
        let bytes = std::fs::read(directory.join(name))
            .map_err(|error| cannot(format!("{}: {error}", name.escape_debug())))?;
        let library = plan::read(name, &bytes)
            .map_err(|error| cannot(format!("{}: {error}", name.escape_debug())))?;
        libraries.push(library);
    }
    let tables: Vec<_> = uxdroid_ndk::tables().into_iter().collect();
    for library in &libraries {
        let missing = library
            .needed
            .iter()
            .find(|needed| !names.contains(needed) && !plan::provided(needed, &tables));
        if let Some(missing) = missing {
            return Err(cannot(format!(
                "{} needs {}, which is neither in the APK nor a system library uxdroid provides",
                library.name.escape_debug(),
                missing.escape_debug()
            )));
        }
    }
    let plans = plan::plan(&libraries, &tables);

    let mut summary = Summary::default();
    for library in &plans {
        let mut unresolved = Vec::new();
        let mut unimplemented = Vec::new();
        for import in &library.imports {
            match &import.binding {
                Binding::App(_) => summary.app += 1,
                Binding::Table(_) => summary.tables += 1,
                Binding::Libc => summary.libc += 1,
                Binding::Stub(_) => summary.stubs.push(import.name.clone()),
                Binding::Unimplemented(_) => unimplemented.push(import.name.clone()),
                Binding::Unresolved if import.weak => summary.weak_null += 1,
                Binding::Unresolved => unresolved.push(import.name.escape_debug().to_string()),
            }
        }
        if !unresolved.is_empty() {
            return Err(cannot(format!(
                "{} imports what no library, table or the guest libc has: {}",
                library.name.escape_debug(),
                unresolved.join(", ")
            )));
        }
        if !unimplemented.is_empty() {
            unimplemented.sort();
            unimplemented.dedup();
            summary
                .unimplemented
                .push((library.name.clone(), unimplemented));
        }
    }
    summary.stubs.sort();
    summary.stubs.dedup();

    let document = json!({
        "abi": GUEST_ABI,
        "host": "x86_64",
        "libraries": plans
            .iter()
            .map(|library| (library.name.clone(), library_json(library)))
            .collect::<serde_json::Map<String, Value>>(),
    });
    let path = directory.join(PLAN_FILE);
    let text =
        serde_json::to_string_pretty(&document).map_err(|error| cannot(error.to_string()))?;
    std::fs::write(&path, text + "\n")
        .map_err(|error| cannot(format!("{}: {error}", path.display())))?;
    Ok(summary)
}
