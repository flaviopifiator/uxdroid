//! Key codes, meta state and key character maps.
//!
//! A Wayland key is a Linux evdev code. Android maps evdev codes to key codes by position with
//! `Generic.kl`, and key codes to characters per layout with a key character map. The table below
//! follows `Generic.kl` from AOSP (Apache-2.0) for the keys a desktop keyboard has. The characters
//! come from the compositor's keymap, through [`super::charmap`].

/// `KeyEvent.KEYCODE_*` used by name elsewhere.
pub const KEYCODE_BACK: i32 = 4;
pub const KEYCODE_ESCAPE: i32 = 111;
pub const KEYCODE_FORWARD: i32 = 125;

/// `KeyEvent.ACTION_*` and `FLAG_*`.
pub const ACTION_DOWN: i32 = 0;
pub const ACTION_UP: i32 = 1;
pub const FLAG_FROM_SYSTEM: i32 = 0x8;
pub const FLAG_FALLBACK: i32 = 0x400;

/// `KeyEvent.META_*`.
pub const META_SHIFT_ON: i32 = 0x1;
pub const META_ALT_ON: i32 = 0x2;
pub const META_ALT_LEFT_ON: i32 = 0x10;
pub const META_ALT_RIGHT_ON: i32 = 0x20;
pub const META_SHIFT_LEFT_ON: i32 = 0x40;
pub const META_CTRL_ON: i32 = 0x1000;
pub const META_CTRL_LEFT_ON: i32 = 0x2000;
pub const META_META_ON: i32 = 0x10000;
pub const META_META_LEFT_ON: i32 = 0x20000;
pub const META_CAPS_LOCK_ON: i32 = 0x100000;
pub const META_NUM_LOCK_ON: i32 = 0x200000;

/// What a key types: characters from the keymap, a fixed character, or nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Its characters depend on the layout.
    Layout,
    /// It types this character on every layout, as `Generic.kcm` says.
    Fixed(char),
    /// Modifiers, navigation and function keys.
    Control,
}

/// One row of the table: evdev code, Android key code, the key code's label, and its kind.
pub struct Mapping {
    pub evdev: u32,
    pub key_code: i32,
    pub label: &'static str,
    pub kind: Kind,
}

const fn key(evdev: u32, key_code: i32, label: &'static str, kind: Kind) -> Mapping {
    Mapping {
        evdev,
        key_code,
        label,
        kind,
    }
}

use Kind::{Control, Fixed, Layout};

/// From `Generic.kl`. KEY_ESC is `ESCAPE`, whose fallback is `BACK` as in `Generic.kcm`.
pub static TABLE: &[Mapping] = &[
    key(1, KEYCODE_ESCAPE, "ESCAPE", Control),
    key(2, 8, "1", Layout),
    key(3, 9, "2", Layout),
    key(4, 10, "3", Layout),
    key(5, 11, "4", Layout),
    key(6, 12, "5", Layout),
    key(7, 13, "6", Layout),
    key(8, 14, "7", Layout),
    key(9, 15, "8", Layout),
    key(10, 16, "9", Layout),
    key(11, 7, "0", Layout),
    key(12, 69, "MINUS", Layout),
    key(13, 70, "EQUALS", Layout),
    key(14, 67, "DEL", Control),
    key(15, 61, "TAB", Fixed('\t')),
    key(16, 45, "Q", Layout),
    key(17, 51, "W", Layout),
    key(18, 33, "E", Layout),
    key(19, 46, "R", Layout),
    key(20, 48, "T", Layout),
    key(21, 53, "Y", Layout),
    key(22, 49, "U", Layout),
    key(23, 37, "I", Layout),
    key(24, 43, "O", Layout),
    key(25, 44, "P", Layout),
    key(26, 71, "LEFT_BRACKET", Layout),
    key(27, 72, "RIGHT_BRACKET", Layout),
    key(28, 66, "ENTER", Fixed('\n')),
    key(29, 113, "CTRL_LEFT", Control),
    key(30, 29, "A", Layout),
    key(31, 47, "S", Layout),
    key(32, 32, "D", Layout),
    key(33, 34, "F", Layout),
    key(34, 35, "G", Layout),
    key(35, 36, "H", Layout),
    key(36, 38, "J", Layout),
    key(37, 39, "K", Layout),
    key(38, 40, "L", Layout),
    key(39, 74, "SEMICOLON", Layout),
    key(40, 75, "APOSTROPHE", Layout),
    key(41, 68, "GRAVE", Layout),
    key(42, 59, "SHIFT_LEFT", Control),
    key(43, 73, "BACKSLASH", Layout),
    key(44, 54, "Z", Layout),
    key(45, 52, "X", Layout),
    key(46, 31, "C", Layout),
    key(47, 50, "V", Layout),
    key(48, 30, "B", Layout),
    key(49, 42, "N", Layout),
    key(50, 41, "M", Layout),
    key(51, 55, "COMMA", Layout),
    key(52, 56, "PERIOD", Layout),
    key(53, 76, "SLASH", Layout),
    key(54, 60, "SHIFT_RIGHT", Control),
    key(55, 155, "NUMPAD_MULTIPLY", Fixed('*')),
    key(56, 57, "ALT_LEFT", Control),
    key(57, 62, "SPACE", Fixed(' ')),
    key(58, 115, "CAPS_LOCK", Control),
    key(59, 131, "F1", Control),
    key(60, 132, "F2", Control),
    key(61, 133, "F3", Control),
    key(62, 134, "F4", Control),
    key(63, 135, "F5", Control),
    key(64, 136, "F6", Control),
    key(65, 137, "F7", Control),
    key(66, 138, "F8", Control),
    key(67, 139, "F9", Control),
    key(68, 140, "F10", Control),
    key(69, 143, "NUM_LOCK", Control),
    key(70, 116, "SCROLL_LOCK", Control),
    key(71, 151, "NUMPAD_7", Fixed('7')),
    key(72, 152, "NUMPAD_8", Fixed('8')),
    key(73, 153, "NUMPAD_9", Fixed('9')),
    key(74, 156, "NUMPAD_SUBTRACT", Fixed('-')),
    key(75, 148, "NUMPAD_4", Fixed('4')),
    key(76, 149, "NUMPAD_5", Fixed('5')),
    key(77, 150, "NUMPAD_6", Fixed('6')),
    key(78, 157, "NUMPAD_ADD", Fixed('+')),
    key(79, 145, "NUMPAD_1", Fixed('1')),
    key(80, 146, "NUMPAD_2", Fixed('2')),
    key(81, 147, "NUMPAD_3", Fixed('3')),
    key(82, 144, "NUMPAD_0", Fixed('0')),
    key(83, 158, "NUMPAD_DOT", Fixed('.')),
    key(86, 73, "BACKSLASH", Layout),
    key(87, 141, "F11", Control),
    key(88, 142, "F12", Control),
    key(96, 160, "NUMPAD_ENTER", Fixed('\n')),
    key(97, 114, "CTRL_RIGHT", Control),
    key(98, 154, "NUMPAD_DIVIDE", Fixed('/')),
    key(99, 120, "SYSRQ", Control),
    key(100, 58, "ALT_RIGHT", Control),
    key(102, 122, "MOVE_HOME", Control),
    key(103, 19, "DPAD_UP", Control),
    key(104, 92, "PAGE_UP", Control),
    key(105, 21, "DPAD_LEFT", Control),
    key(106, 22, "DPAD_RIGHT", Control),
    key(107, 123, "MOVE_END", Control),
    key(108, 20, "DPAD_DOWN", Control),
    key(109, 93, "PAGE_DOWN", Control),
    key(110, 124, "INSERT", Control),
    key(111, 112, "FORWARD_DEL", Control),
    key(113, 164, "VOLUME_MUTE", Control),
    key(114, 25, "VOLUME_DOWN", Control),
    key(115, 24, "VOLUME_UP", Control),
    key(116, 26, "POWER", Control),
    key(117, 161, "NUMPAD_EQUALS", Fixed('=')),
    key(119, 121, "BREAK", Control),
    key(121, 159, "NUMPAD_COMMA", Fixed(',')),
    key(125, 117, "META_LEFT", Control),
    key(126, 118, "META_RIGHT", Control),
    key(127, 82, "MENU", Control),
    key(158, KEYCODE_BACK, "BACK", Control),
    key(163, 87, "MEDIA_NEXT", Control),
    key(164, 85, "MEDIA_PLAY_PAUSE", Control),
    key(165, 88, "MEDIA_PREVIOUS", Control),
    key(166, 86, "MEDIA_STOP", Control),
    key(172, 3, "HOME", Control),
    key(217, 84, "SEARCH", Control),
];

/// The table row of an evdev code.
pub fn mapping(evdev: u32) -> Option<&'static Mapping> {
    TABLE.iter().find(|mapping| mapping.evdev == evdev)
}

/// The key `KeyCharacterMap.getFallbackAction` names when the app does not handle `key_code`, as
/// `Generic.kcm` defines it for keys with no meta state.
pub fn fallback(key_code: i32) -> Option<i32> {
    (key_code == KEYCODE_ESCAPE).then_some(KEYCODE_BACK)
}

/// Which xkb modifier index means what; `None` when the keymap lacks the modifier.
#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub shift: Option<u32>,
    pub caps: Option<u32>,
    pub ctrl: Option<u32>,
    pub alt: Option<u32>,
    pub num: Option<u32>,
    pub logo: Option<u32>,
    /// AltGr: `ralt` in a key character map.
    pub level3: Option<u32>,
}

/// Android's meta state for the masks of `wl_keyboard.modifiers`.
pub fn meta_state(indices: &Modifiers, depressed: u32, latched: u32, locked: u32) -> i32 {
    let held = depressed | latched;
    let set = |index: Option<u32>, mask: u32| index.is_some_and(|i| i < 32 && mask & 1 << i != 0);
    let mut meta = 0;
    // Wayland names no side; Android's key character maps use the left one unless it is AltGr.
    if set(indices.shift, held) {
        meta |= META_SHIFT_ON | META_SHIFT_LEFT_ON;
    }
    if set(indices.ctrl, held) {
        meta |= META_CTRL_ON | META_CTRL_LEFT_ON;
    }
    if set(indices.alt, held) {
        meta |= META_ALT_ON | META_ALT_LEFT_ON;
    }
    if set(indices.level3, held) {
        meta |= META_ALT_ON | META_ALT_RIGHT_ON;
    }
    if set(indices.logo, held) {
        meta |= META_META_ON | META_META_LEFT_ON;
    }
    if set(indices.caps, locked) {
        meta |= META_CAPS_LOCK_ON;
    }
    if set(indices.num, locked) {
        meta |= META_NUM_LOCK_ON;
    }
    meta
}

/// The characters of one key at each level of the layout; `None` types nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Levels {
    pub base: Option<char>,
    pub shift: Option<char>,
    pub caps: Option<char>,
    pub shift_caps: Option<char>,
    pub ralt: Option<char>,
    pub shift_ralt: Option<char>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_codes_map_to_android_key_codes_by_position() {
        let a = mapping(30).expect("KEY_A");
        assert_eq!((a.key_code, a.label), (29, "A"));
        assert_eq!(mapping(28).map(|m| m.key_code), Some(66), "KEY_ENTER");
        assert_eq!(
            mapping(14).map(|m| m.key_code),
            Some(67),
            "KEY_BACKSPACE is DEL"
        );
        assert_eq!(mapping(1).map(|m| m.key_code), Some(KEYCODE_ESCAPE));
        assert_eq!(mapping(158).map(|m| m.key_code), Some(KEYCODE_BACK));
        assert!(mapping(0).is_none());
        assert!(mapping(u32::MAX).is_none());
    }

    #[test]
    fn every_evdev_code_appears_once() {
        let mut codes: Vec<u32> = TABLE.iter().map(|m| m.evdev).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), TABLE.len());
    }

    #[test]
    fn escape_falls_back_to_back_as_in_generic_kcm() {
        assert_eq!(fallback(KEYCODE_ESCAPE), Some(KEYCODE_BACK));
        assert_eq!(fallback(29), None);
    }

    #[test]
    fn modifier_masks_become_meta_state() {
        let indices = Modifiers {
            shift: Some(0),
            caps: Some(1),
            ctrl: Some(2),
            alt: Some(3),
            num: Some(4),
            logo: Some(6),
            level3: Some(7),
        };
        assert_eq!(meta_state(&indices, 0, 0, 0), 0);
        assert_eq!(
            meta_state(&indices, 1, 0, 0),
            META_SHIFT_ON | META_SHIFT_LEFT_ON
        );
        // A latched shift counts as held; caps and num lock count only when locked.
        assert_eq!(
            meta_state(&indices, 0, 1, 1 << 1 | 1 << 4),
            META_SHIFT_ON | META_SHIFT_LEFT_ON | META_CAPS_LOCK_ON | META_NUM_LOCK_ON
        );
        assert_eq!(
            meta_state(&indices, 1 << 2 | 1 << 3, 0, 0),
            META_CTRL_ON | META_CTRL_LEFT_ON | META_ALT_ON | META_ALT_LEFT_ON
        );
        assert_eq!(
            meta_state(&indices, 1 << 7, 0, 0),
            META_ALT_ON | META_ALT_RIGHT_ON
        );
        assert_eq!(
            meta_state(&indices, 1 << 6, 0, 0),
            META_META_ON | META_META_LEFT_ON
        );
        // A keymap without the modifier ignores its bit.
        assert_eq!(meta_state(&Modifiers::default(), u32::MAX, 0, u32::MAX), 0);
    }
}
