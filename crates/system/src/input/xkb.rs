//! The compositor's keymap, read with libxkbcommon.
//!
//! `wl_keyboard.keymap` hands the client the keymap the compositor uses, in xkb's text format. Its
//! characters per key and level fill the key character map of [`super::charmap`], and its modifier indices turn
//! `wl_keyboard.modifiers` into Android's meta state. libxkbcommon is opened at run time, the first
//! time a keymap is read, so a process that never types does not load it, and a host without it
//! still gets key codes, only no characters.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

use super::keys::{Levels, Modifiers};

/// The functions of libxkbcommon this module calls.
struct Library {
    context_new: unsafe extern "C" fn(c_int) -> *mut c_void,
    context_unref: unsafe extern "C" fn(*mut c_void),
    keymap_from_string:
        unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_int) -> *mut c_void,
    keymap_from_names: unsafe extern "C" fn(*mut c_void, *const RuleNames, c_int) -> *mut c_void,
    keymap_unref: unsafe extern "C" fn(*mut c_void),
    keymap_mod_index: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
    keymap_as_string: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char,
    state_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    state_unref: unsafe extern "C" fn(*mut c_void),
    state_update_mask: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, u32) -> c_int,
    state_key_utf32: unsafe extern "C" fn(*mut c_void, u32) -> u32,
}

#[repr(C)]
struct RuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

const KEYMAP_FORMAT_TEXT_V1: c_int = 1;
const MOD_INVALID: u32 = u32::MAX;
/// xkb key codes are evdev codes plus 8.
const EVDEV_OFFSET: u32 = 8;

fn library() -> Option<&'static Library> {
    static LIBRARY: OnceLock<Option<Library>> = OnceLock::new();
    LIBRARY.get_or_init(open_library).as_ref()
}

fn open_library() -> Option<Library> {
    // SAFETY: dlopen of a system library by its soname; its initializers are xkb's own.
    let handle = unsafe {
        libc::dlopen(
            c"libxkbcommon.so.0".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        )
    };
    if handle.is_null() {
        eprintln!("uxdroid: no libxkbcommon.so.0, so keys type no characters");
        return None;
    }
    /// A function of the library, as the field's type that receives it.
    ///
    /// # Safety
    /// T must be the function pointer type of the C signature of `name`.
    unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
        // SAFETY: `handle` is live, and the name is a C string.
        let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
        // SAFETY: T is the function pointer type of this symbol, the size of a pointer; the caller
        // names the symbol whose C signature T declares.
        (!address.is_null())
            .then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
    }
    // SAFETY: each symbol is libxkbcommon's function of that name, whose C signature the field
    // declares, as in xkbcommon.h.
    unsafe {
        Some(Library {
            context_new: symbol(handle, c"xkb_context_new")?,
            context_unref: symbol(handle, c"xkb_context_unref")?,
            keymap_from_string: symbol(handle, c"xkb_keymap_new_from_string")?,
            keymap_from_names: symbol(handle, c"xkb_keymap_new_from_names")?,
            keymap_unref: symbol(handle, c"xkb_keymap_unref")?,
            keymap_mod_index: symbol(handle, c"xkb_keymap_mod_get_index")?,
            keymap_as_string: symbol(handle, c"xkb_keymap_get_as_string")?,
            state_new: symbol(handle, c"xkb_state_new")?,
            state_unref: symbol(handle, c"xkb_state_unref")?,
            state_update_mask: symbol(handle, c"xkb_state_update_mask")?,
            state_key_utf32: symbol(handle, c"xkb_state_key_get_utf32")?,
        })
    }
}

/// A compiled keymap.
pub struct Keymap {
    library: &'static Library,
    context: *mut c_void,
    keymap: *mut c_void,
    state: *mut c_void,
}

// SAFETY: the pointers are this Keymap's own xkb objects, used only through `&mut self` or by one
// thread at a time behind the owner's lock.
unsafe impl Send for Keymap {}

impl Drop for Keymap {
    fn drop(&mut self) {
        // SAFETY: each object is this Keymap's own reference.
        unsafe {
            (self.library.state_unref)(self.state);
            (self.library.keymap_unref)(self.keymap);
            (self.library.context_unref)(self.context);
        }
    }
}

impl Keymap {
    /// Compiles a keymap in xkb's text format; `None` when it is invalid or xkb is missing.
    pub fn from_text(text: &str) -> Option<Keymap> {
        let text = CString::new(text).ok()?;
        Keymap::compile(|library, context| {
            // SAFETY: a live context and a C string; xkb copies what it keeps.
            unsafe {
                (library.keymap_from_string)(context, text.as_ptr(), KEYMAP_FORMAT_TEXT_V1, 0)
            }
        })
    }

    /// Compiles the keymap of a layout name such as `us` or `de`, from the host's xkb data.
    pub fn from_layout(layout: &str) -> Option<Keymap> {
        let layout = CString::new(layout).ok()?;
        let names = RuleNames {
            rules: c"evdev".as_ptr(),
            model: c"pc105".as_ptr(),
            layout: layout.as_ptr(),
            variant: c"".as_ptr(),
            options: c"".as_ptr(),
        };
        Keymap::compile(|library, context| {
            // SAFETY: a live context and rule names whose strings outlive the call.
            unsafe { (library.keymap_from_names)(context, &names, 0) }
        })
    }

    fn compile(make: impl FnOnce(&Library, *mut c_void) -> *mut c_void) -> Option<Keymap> {
        let library = library()?;
        // SAFETY: plain constructor calls; every result is checked and owned by the Keymap.
        unsafe {
            let context = (library.context_new)(0);
            if context.is_null() {
                return None;
            }
            let keymap = make(library, context);
            if keymap.is_null() {
                (library.context_unref)(context);
                return None;
            }
            let state = (library.state_new)(keymap);
            if state.is_null() {
                (library.keymap_unref)(keymap);
                (library.context_unref)(context);
                return None;
            }
            Some(Keymap {
                library,
                context,
                keymap,
                state,
            })
        }
    }

    fn index(&self, name: &CStr) -> Option<u32> {
        // SAFETY: a live keymap and a C string.
        let index = unsafe { (self.library.keymap_mod_index)(self.keymap, name.as_ptr()) };
        (index != MOD_INVALID && index < 32).then_some(index)
    }

    /// The keymap in xkb's text format, as a compositor sends it.
    pub fn text(&self) -> Option<String> {
        // SAFETY: a live keymap; the string is malloc'd for the caller, copied, then freed.
        unsafe {
            let text = (self.library.keymap_as_string)(self.keymap, KEYMAP_FORMAT_TEXT_V1);
            if text.is_null() {
                return None;
            }
            let owned = CStr::from_ptr(text).to_string_lossy().into_owned();
            libc::free(text.cast());
            Some(owned)
        }
    }

    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.index(c"Shift"),
            caps: self.index(c"Lock"),
            ctrl: self.index(c"Control"),
            alt: self.index(c"Mod1"),
            num: self.index(c"Mod2"),
            logo: self.index(c"Mod4"),
            level3: self.index(c"Mod5"),
        }
    }

    /// The character an evdev key types with these modifiers held and locked.
    fn character(&mut self, evdev: u32, depressed: u32, locked: u32) -> Option<char> {
        // SAFETY: a live state; the key code is any number, which xkb bounds itself.
        let code = unsafe {
            (self.library.state_update_mask)(self.state, depressed, 0, locked, 0, 0, 0);
            (self.library.state_key_utf32)(self.state, evdev.checked_add(EVDEV_OFFSET)?)
        };
        char::from_u32(code).filter(|c| !c.is_control())
    }

    /// The characters of an evdev key at each level.
    pub fn levels(&mut self, evdev: u32) -> Levels {
        let modifiers = self.modifiers();
        let bit = |index: Option<u32>| index.map_or(0, |i| 1u32 << i);
        let (shift, caps, ralt) = (
            bit(modifiers.shift),
            bit(modifiers.caps),
            bit(modifiers.level3),
        );
        let levels = Levels {
            base: self.character(evdev, 0, 0),
            shift: (shift != 0)
                .then(|| self.character(evdev, shift, 0))
                .flatten(),
            caps: (caps != 0)
                .then(|| self.character(evdev, 0, caps))
                .flatten(),
            shift_caps: (shift != 0 && caps != 0)
                .then(|| self.character(evdev, shift, caps))
                .flatten(),
            ralt: (ralt != 0)
                .then(|| self.character(evdev, ralt, 0))
                .flatten(),
            shift_ralt: (shift != 0 && ralt != 0)
                .then(|| self.character(evdev, shift | ralt, 0))
                .flatten(),
        };
        // Leave the state as no modifiers, for the next caller.
        self.character(evdev, 0, 0);
        levels
    }
}

/// A keymap that needs no layouts from the host: a few letters with a German Y and Z, a digit with
/// its shifted symbol, AltGr levels on Q, and the modifiers, for tests.
#[cfg(test)]
pub(crate) const FIXTURE: &str = r#"xkb_keymap {
xkb_keycodes "fixture" {
    minimum = 8;
    maximum = 255;
    <ESC> = 9; <AE02> = 11; <AD01> = 24; <AD06> = 29; <LCTL> = 37; <AC01> = 38;
    <LFSH> = 50; <AB01> = 52; <CAPS> = 66; <RALT> = 108;
};
xkb_types "fixture" {
    type "ONE_LEVEL" { modifiers = none; level_name[Level1] = "Any"; };
    type "TWO_LEVEL" {
        modifiers = Shift; map[Shift] = Level2;
        level_name[Level1] = "Base"; level_name[Level2] = "Shift";
    };
    type "ALPHABETIC" {
        modifiers = Shift+Lock; map[Shift] = Level2; map[Lock] = Level2;
        level_name[Level1] = "Base"; level_name[Level2] = "Caps";
    };
    type "ALPHABETIC_LEVEL3" {
        modifiers = Shift+Lock+Mod5; map[Shift] = Level2; map[Lock] = Level2;
        map[Mod5] = Level3; map[Shift+Mod5] = Level4;
        level_name[Level1] = "Base"; level_name[Level2] = "Caps";
        level_name[Level3] = "AltGr"; level_name[Level4] = "Shift AltGr";
    };
};
xkb_compatibility "fixture" { };
xkb_symbols "fixture" {
    key <ESC> { [ Escape ] };
    key <AE02> { type = "TWO_LEVEL", [ 2, at ] };
    key <AD01> { type = "ALPHABETIC_LEVEL3", [ q, Q, at, Greek_OMEGA ] };
    key <AD06> { type = "ALPHABETIC", [ z, Z ] };
    key <AC01> { type = "ALPHABETIC", [ a, A ] };
    key <AB01> { type = "ALPHABETIC", [ y, Y ] };
    key <LFSH> { [ Shift_L ] };
    key <CAPS> { [ Caps_Lock ] };
    key <LCTL> { [ Control_L ] };
    key <RALT> { [ ISO_Level3_Shift ] };
    modifier_map Shift { <LFSH> };
    modifier_map Lock { <CAPS> };
    modifier_map Control { <LCTL> };
    modifier_map Mod5 { <RALT> };
};
};
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keymap_types_its_levels_with_shift_caps_lock_and_altgr() {
        let mut map = Keymap::from_text(FIXTURE).expect("xkbcommon compiles the fixture");
        let a = map.levels(30);
        assert_eq!(a.base, Some('a'));
        assert_eq!(a.shift, Some('A'));
        assert_eq!(a.caps, Some('A'));
        assert_eq!(a.shift_caps, Some('a'));
        let two = map.levels(3);
        assert_eq!(
            (two.base, two.shift, two.caps),
            (Some('2'), Some('@'), Some('2'))
        );
        assert_eq!(map.levels(1).base, None, "Escape types nothing");
        // KEY_Y types z, as on a German keyboard, and AltGr+Q is @.
        assert_eq!(map.levels(21).base, Some('z'));
        assert_eq!(map.levels(44).base, Some('y'));
        let q = map.levels(16);
        assert_eq!((q.ralt, q.shift_ralt), (Some('@'), Some('Ω')));
        let modifiers = map.modifiers();
        assert!(modifiers.shift.is_some() && modifiers.caps.is_some() && modifiers.ctrl.is_some());
    }

    #[test]
    fn a_keymap_survives_its_text_form() {
        let map = Keymap::from_text(FIXTURE).expect("compiles");
        let mut again = Keymap::from_text(&map.text().expect("text")).expect("compiles");
        assert_eq!(again.levels(21).base, Some('z'));
    }

    #[test]
    fn a_host_layout_compiles_when_the_host_has_xkb_data() {
        if !std::path::Path::new("/usr/share/X11/xkb/symbols/us").exists() {
            eprintln!("skipped: no xkb layouts on this host");
            return;
        }
        let mut us = Keymap::from_layout("us").expect("the us layout");
        assert_eq!(us.levels(30).base, Some('a'));
    }

    #[test]
    fn invalid_keymap_text_compiles_to_nothing() {
        assert!(Keymap::from_text("").is_none());
        assert!(Keymap::from_text("xkb_keymap { garbage").is_none());
        assert!(Keymap::from_text("a\0b").is_none());
    }
}
