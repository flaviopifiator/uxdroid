//! The key character map of the seat's keyboard: what `KeyCharacterMap` answers.
//!
//! Android loads a `.kcm` file per keyboard layout. The host library can only make an empty map,
//! whose overlay it cannot apply, so the natives of `KeyCharacterMap` are ours (see
//! [`super::natives`]), and every map reads this one: the characters of the compositor's keymap,
//! per Android key code and level, from [`super::xkb`]. Until the compositor sends a keymap, the
//! host's default layout (`XKB_DEFAULT_LAYOUT`, or `us`) stands in.
//!
//! Lookups follow AOSP's `KeyCharacterMap::getCharacter`: shift and caps lock choose the level, a
//! right alt is AltGr, and control, meta or a left alt type nothing, because no level names them.

use std::sync::{Arc, RwLock};

use super::keys::{
    self, Kind, Levels, META_ALT_LEFT_ON, META_ALT_RIGHT_ON, META_CAPS_LOCK_ON, META_CTRL_ON,
    META_META_ON, META_SHIFT_LEFT_ON, META_SHIFT_ON, TABLE,
};
use super::xkb::Keymap;

/// `KeyCharacterMap.FULL`: a full PC-style keyboard, which TextView's key listener types with.
pub const FULL: i32 = 4;
/// `KeyEvent.KEYCODE_SHIFT_LEFT`.
const KEYCODE_SHIFT_LEFT: i32 = 59;

/// Characters per Android key code.
#[derive(Debug, Default, PartialEq)]
pub struct Charmap {
    keys: Vec<(i32, Levels)>,
}

impl Charmap {
    /// A map from each table key's levels, by evdev code; fixed keys type their fixed character.
    pub fn from_levels(mut levels: impl FnMut(u32) -> Levels) -> Charmap {
        let mut keys: Vec<(i32, Levels)> = Vec::new();
        for mapping in TABLE {
            // Two evdev codes can share a key code, such as KEY_102ND and KEY_BACKSLASH: the first wins.
            if keys.iter().any(|(code, _)| *code == mapping.key_code) {
                continue;
            }
            let found = match mapping.kind {
                Kind::Control => continue,
                Kind::Fixed(character) => Levels {
                    base: Some(character),
                    ..Levels::default()
                },
                Kind::Layout => levels(mapping.evdev),
            };
            if found != Levels::default() {
                keys.push((mapping.key_code, found));
            }
        }
        Charmap { keys }
    }

    pub fn from_keymap(keymap: &mut Keymap) -> Charmap {
        Charmap::from_levels(|evdev| keymap.levels(evdev))
    }

    fn levels(&self, key_code: i32) -> Option<&Levels> {
        self.keys
            .iter()
            .find(|(code, _)| *code == key_code)
            .map(|(_, levels)| levels)
    }

    /// The character a key types with a meta state, 0 for none: `nativeGetCharacter`.
    pub fn character(&self, key_code: i32, meta: i32) -> u16 {
        let Some(levels) = self.levels(key_code) else {
            return 0;
        };
        // AOSP's behaviors match control, meta and alt exactly, and no level here names them.
        if meta & (META_CTRL_ON | META_META_ON | META_ALT_LEFT_ON) != 0 {
            return 0;
        }
        let shift = meta & META_SHIFT_ON != 0;
        let found = if meta & META_ALT_RIGHT_ON != 0 {
            if shift {
                levels.shift_ralt
            } else {
                levels.ralt
            }
        } else {
            let caps = meta & META_CAPS_LOCK_ON != 0;
            let tries = [
                (shift && caps, levels.shift_caps),
                (shift, levels.shift),
                (caps, levels.caps),
                (true, levels.base),
            ];
            tries
                .into_iter()
                .find_map(|(applies, level)| level.filter(|_| applies))
        };
        found.map_or(0, unit)
    }

    /// The label printed on the key: `nativeGetDisplayLabel`.
    pub fn display_label(&self, key_code: i32) -> u16 {
        self.levels(key_code)
            .and_then(|levels| levels.base)
            .map_or(0, |base| {
                unit(
                    base.to_uppercase()
                        .next()
                        .filter(|c| unit(*c) != 0)
                        .unwrap_or(base),
                )
            })
    }

    /// The digit or dial symbol of a key: `nativeGetNumber`.
    pub fn number(&self, key_code: i32) -> u16 {
        let Some(levels) = self.levels(key_code) else {
            return 0;
        };
        [levels.base, levels.shift]
            .into_iter()
            .flatten()
            .find(|c| c.is_ascii_digit() || "*#+,.-".contains(*c))
            .map_or(0, unit)
    }

    /// The first of `chars` the key can type, preferring what it types with `meta`: `nativeGetMatch`.
    pub fn matching(&self, key_code: i32, chars: &[u16], meta: i32) -> u16 {
        let preferred = self.character(key_code, meta);
        if preferred != 0 && chars.contains(&preferred) {
            return preferred;
        }
        let Some(levels) = self.levels(key_code) else {
            return 0;
        };
        let all = [
            levels.base,
            levels.shift,
            levels.caps,
            levels.shift_caps,
            levels.ralt,
            levels.shift_ralt,
        ];
        chars
            .iter()
            .copied()
            .find(|wanted| {
                all.iter()
                    .flatten()
                    .any(|c| unit(*c) == *wanted && *wanted != 0)
            })
            .unwrap_or(0)
    }

    /// Key presses that type `chars`, as (action, key code, meta state), or `None` when one cannot
    /// be typed: `nativeGetEvents`.
    pub fn events(&self, chars: &[u16]) -> Option<Vec<(i32, i32, i32)>> {
        let shift = META_SHIFT_ON | META_SHIFT_LEFT_ON;
        let mut events = Vec::new();
        for wanted in chars {
            let typed = |meta: i32| {
                self.keys
                    .iter()
                    .find(|(code, _)| self.character(*code, meta) == *wanted && *wanted != 0)
                    .map(|(code, _)| *code)
            };
            if let Some(code) = typed(0) {
                events.push((keys::ACTION_DOWN, code, 0));
                events.push((keys::ACTION_UP, code, 0));
            } else {
                let code = typed(shift)?;
                events.push((keys::ACTION_DOWN, KEYCODE_SHIFT_LEFT, shift));
                events.push((keys::ACTION_DOWN, code, shift));
                events.push((keys::ACTION_UP, code, shift));
                events.push((keys::ACTION_UP, KEYCODE_SHIFT_LEFT, 0));
            }
        }
        Some(events)
    }
}

/// A character as Java's `char`; characters outside the basic plane cannot be one, and type nothing.
fn unit(character: char) -> u16 {
    u16::try_from(u32::from(character)).unwrap_or(0)
}

static SEAT: RwLock<Option<Arc<Charmap>>> = RwLock::new(None);

/// The seat's map: the compositor's keymap once it arrived, before that the host's default layout.
pub fn seat() -> Arc<Charmap> {
    if let Some(map) = SEAT.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        return map.clone();
    }
    let layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "us".into());
    let map = Arc::new(match Keymap::from_layout(&layout) {
        Some(mut keymap) => Charmap::from_keymap(&mut keymap),
        None => Charmap::from_levels(|_| Levels::default()),
    });
    let mut seat = SEAT.write().unwrap_or_else(|p| p.into_inner());
    seat.get_or_insert(map).clone()
}

/// Replaces the seat's map with the compositor's keymap.
pub fn set_seat(map: Charmap) {
    *SEAT.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(map));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keys::{META_ALT_ON, META_CTRL_LEFT_ON};

    const A: i32 = 29;
    const TWO: i32 = 9;
    const SPACE: i32 = 62;
    const ENTER: i32 = 66;
    const SHIFT: i32 = META_SHIFT_ON | META_SHIFT_LEFT_ON;

    fn sample() -> Charmap {
        Charmap::from_levels(|evdev| match evdev {
            30 => Levels {
                base: Some('a'),
                shift: Some('A'),
                caps: Some('A'),
                shift_caps: Some('a'),
                ralt: Some('æ'),
                shift_ralt: Some('Æ'),
            },
            3 => Levels {
                base: Some('2'),
                shift: Some('@'),
                caps: None,
                shift_caps: None,
                ralt: Some('²'),
                shift_ralt: None,
            },
            _ => Levels::default(),
        })
    }

    fn c(text: char) -> u16 {
        text as u16
    }

    #[test]
    fn shift_and_caps_lock_choose_the_level() {
        let map = sample();
        assert_eq!(map.character(A, 0), c('a'));
        assert_eq!(map.character(A, SHIFT), c('A'));
        assert_eq!(map.character(A, META_CAPS_LOCK_ON), c('A'));
        assert_eq!(map.character(A, SHIFT | META_CAPS_LOCK_ON), c('a'));
        // A level the keymap lacks falls back as AOSP's behaviors do: shift, then caps, then base.
        assert_eq!(map.character(TWO, META_CAPS_LOCK_ON), c('2'));
        assert_eq!(map.character(TWO, SHIFT | META_CAPS_LOCK_ON), c('@'));
    }

    #[test]
    fn altgr_is_right_alt_and_other_modifiers_type_nothing() {
        let map = sample();
        let altgr = META_ALT_ON | META_ALT_RIGHT_ON;
        assert_eq!(map.character(A, altgr), c('æ'));
        assert_eq!(map.character(A, altgr | SHIFT), c('Æ'));
        assert_eq!(map.character(TWO, altgr | SHIFT), 0, "no shift+ralt level");
        assert_eq!(map.character(A, META_ALT_ON | META_ALT_LEFT_ON), 0);
        assert_eq!(map.character(A, META_CTRL_ON | META_CTRL_LEFT_ON), 0);
        assert_eq!(map.character(A, META_META_ON), 0);
    }

    #[test]
    fn fixed_keys_type_on_every_layout_and_unknown_keys_type_nothing() {
        let map = Charmap::from_levels(|_| Levels::default());
        assert_eq!(map.character(SPACE, 0), c(' '));
        assert_eq!(map.character(ENTER, SHIFT), c('\n'));
        assert_eq!(map.character(A, 0), 0);
        assert_eq!(map.character(-1, 0), 0);
        assert_eq!(map.character(keys::KEYCODE_ESCAPE, 0), 0);
    }

    #[test]
    fn labels_numbers_and_matches() {
        let map = sample();
        assert_eq!(map.display_label(A), c('A'));
        assert_eq!(map.display_label(TWO), c('2'));
        assert_eq!(map.number(TWO), c('2'));
        assert_eq!(map.number(A), 0);
        assert_eq!(map.matching(A, &[c('x'), c('A'), c('a')], 0), c('a'));
        assert_eq!(map.matching(A, &[c('x'), c('A')], 0), c('A'));
        assert_eq!(map.matching(A, &[c('x')], 0), 0);
    }

    #[test]
    fn text_becomes_key_presses_with_shift_when_needed() {
        let map = sample();
        let down = keys::ACTION_DOWN;
        let up = keys::ACTION_UP;
        assert_eq!(
            map.events(&[c('a'), c('@')]),
            Some(vec![
                (down, A, 0),
                (up, A, 0),
                (down, KEYCODE_SHIFT_LEFT, SHIFT),
                (down, TWO, SHIFT),
                (up, TWO, SHIFT),
                (up, KEYCODE_SHIFT_LEFT, 0),
            ])
        );
        assert_eq!(map.events(&[c('a'), c('€')]), None);
        assert_eq!(map.events(&[]), Some(Vec::new()));
    }

    #[test]
    fn the_compositor_s_layout_decides_the_characters() {
        let mut de = Keymap::from_layout("de").expect("the de layout");
        let map = Charmap::from_keymap(&mut de);
        // KEYCODE_Y sits where a German keyboard has Z.
        assert_eq!(map.character(53, 0), c('z'));
        assert_eq!(map.character(45, META_ALT_ON | META_ALT_RIGHT_ON), c('@'));
    }
}
