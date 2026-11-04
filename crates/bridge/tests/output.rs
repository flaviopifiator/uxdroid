//! The bridge's output, linked as the rlib other libraries link: its copy of stderr is made when the
//! program loads, before main, from the initializer the library carries.

#[test]
fn stderr_is_kept_when_the_library_loads() {
    let fd = uxdroid_bridge::output::stderr_fd();
    assert!(
        fd > 2,
        "messages go to descriptor {fd}, not to a copy kept at load"
    );
}
