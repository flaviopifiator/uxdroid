//! The project's mark, the antenna prompt, as each surface can show it: terminal art for `--help`,
//! the accent color for the terminal UI and the SVG icon for MCP clients.

/// The mark's green, `#2ed3a3` in `docs/logo`.
pub(crate) const ACCENT: (u8, u8, u8) = (0x2e, 0xd3, 0xa3);

/// The mark drawn with box characters: two antennas over a window with a prompt.
const MARK: [&str; 6] = [
    "  \u{2022}      \u{2022}",
    "   \u{2572}    \u{2571}",
    " \u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
    " \u{2502} >      \u{2502}",
    " \u{2502}    ___ \u{2502}",
    " \u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
];

/// What sits beside the mark, by line.
const BESIDE: [&str; 6] = [
    "",
    "",
    "",
    "uxdroid",
    "Run Android apps on Linux as native processes.",
    "",
];

const ICON_SVG: &str = include_str!("../../../docs/logo/icon.svg");

/// The mark with the program's name and purpose, in the accent color when `color` is set.
pub(crate) fn banner(color: bool) -> String {
    let (red, green, blue) = ACCENT;
    let (paint, bold, reset) = if color {
        (
            format!("\x1b[38;2;{red};{green};{blue}m"),
            "\x1b[1m",
            "\x1b[0m",
        )
    } else {
        (String::new(), "", "")
    };
    let mut text = String::new();
    for (index, (mark, beside)) in MARK.iter().zip(BESIDE).enumerate() {
        text.push_str(&format!("{paint}{mark}{reset}"));
        match (index, beside.is_empty()) {
            (_, true) => {}
            (3, false) => text.push_str(&format!("  {bold}{beside}{reset}")),
            _ => text.push_str(&format!("  {beside}")),
        }
        text.push('\n');
    }
    text
}

/// The app icon as a `data:` URI, percent-encoded so any client can read it without base64.
pub(crate) fn icon_data_uri() -> String {
    let mut uri = String::from("data:image/svg+xml,");
    for byte in ICON_SVG.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/=:;,()".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}
