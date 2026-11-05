//! How each path binds a system library's symbol, as the coverage audit classifies it.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::ffi::c_int;

use uxdroid_bridge::guest::plan::{self, Binding, GuestLibrary, Symbol};
use uxdroid_bridge::table::{Export, Table};
use uxdroid_bridge::{HostBinding, host_binding};

extern "C" fn answer() -> c_int {
    42
}

static COVERAGE_TABLE: Table = Table {
    library: "libuxdroidcoverage.so",
    exports: &[Export::function(
        c"coverage_answer",
        "i32()",
        answer as *const (),
    )],
};

#[test]
fn a_table_export_binds_to_its_table() {
    assert_eq!(
        host_binding("coverage_answer", &[&COVERAGE_TABLE], false),
        HostBinding::Table("libuxdroidcoverage.so")
    );
}

#[test]
fn liblog_is_searched_like_the_loader_does() {
    assert_eq!(
        host_binding("__android_log_print", &[], false),
        HostBinding::Table("liblog.so")
    );
}

#[test]
fn an_audited_glibc_function_binds_to_glibc() {
    assert_eq!(host_binding("strlen", &[], false), HostBinding::Glibc);
}

#[test]
fn a_bionic_name_glibc_spells_differently_binds_to_a_shim() {
    assert_eq!(host_binding("__errno", &[], false), HostBinding::Shim);
}

#[test]
fn an_incompatible_abi_binds_to_a_trap() {
    assert!(matches!(
        host_binding("fgetpos", &[], false),
        HostBinding::Trap(_)
    ));
}

#[test]
fn an_unaudited_glibc_function_binds_to_a_trap() {
    assert!(matches!(
        host_binding("gnu_get_libc_version", &[], false),
        HostBinding::Trap(_)
    ));
}

#[test]
fn unaudited_glibc_data_cannot_trap_and_is_missing() {
    assert!(matches!(
        host_binding("program_invocation_name", &[], true),
        HostBinding::Missing(_)
    ));
}

#[test]
fn a_name_found_nowhere_is_missing() {
    assert!(matches!(
        host_binding("uxdroid_coverage_nowhere", &[], false),
        HostBinding::Missing(_)
    ));
}

/// The arm64 planner's choice for each name, for a guest library that needs only `needed`.
fn guest_bindings(needed: &str, names: &[(&str, bool)], tables: &[&'static Table]) -> Vec<Binding> {
    let probe = GuestLibrary {
        name: "libcoverageprobe.so".to_string(),
        needed: vec![needed.to_string()],
        symbols: names
            .iter()
            .map(|&(name, object)| Symbol {
                name: name.to_string(),
                defined: false,
                weak: false,
                function: !object,
                object,
                value: 0,
                size: 0,
            })
            .collect(),
    };
    plan::plan(&[probe], tables)
        .remove(0)
        .imports
        .into_iter()
        .map(|import| import.binding)
        .collect()
}

#[test]
fn the_planner_classifies_a_system_library_without_app_libraries() {
    let bindings = guest_bindings(
        "libuxdroidcoverage.so",
        &[
            ("coverage_answer", false),
            ("strlen", false),
            ("uxdroid_coverage_nowhere", false),
        ],
        &[&COVERAGE_TABLE],
    );
    assert_eq!(
        bindings,
        [
            Binding::Table("libuxdroidcoverage.so"),
            Binding::Libc,
            Binding::Unresolved
        ]
    );
}
