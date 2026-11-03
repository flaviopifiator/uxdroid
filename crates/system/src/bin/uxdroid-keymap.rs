//! Prints the xkb keymap of a layout, such as `us` or `de`, as a compositor sends it. The input smoke
//! tests' compositor hands it to apps.
fn main() {
    let layout = std::env::args().nth(1).unwrap_or_else(|| "us".into());
    match uxdroid_system::input::xkb::Keymap::from_layout(&layout).and_then(|keymap| keymap.text())
    {
        Some(text) => print!("{text}"),
        None => {
            eprintln!("no keymap for layout {layout}");
            std::process::exit(1);
        }
    }
}
