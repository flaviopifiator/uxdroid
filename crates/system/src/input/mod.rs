//! Input from the Wayland seat to the framework's `InputEventReceiver`.
//!
//! On Android, the InputReader turns kernel events into key and motion events, InputDispatcher
//! writes them as `InputMessage`s into the `InputChannel` of the focused or touched window, and the
//! window's `InputEventReceiver` reads them on its Looper. Here the Wayland compositor already did
//! the reader's part, so each window's seat events become Android events in [`translate`], their
//! key codes come from [`keys`], their characters from the compositor's keymap ([`xkb`]) through
//! [`charmap`], and they cross the window's channel, a socket pair, as the records of [`wire`].
//! [`natives`] implements `InputChannel`, `InputEventReceiver` and `KeyCharacterMap` over them.

pub mod charmap;
pub mod keys;
pub mod natives;
pub mod translate;
pub mod wire;
pub mod xkb;

/// `InputDevice` ids of the seat's devices, which `dev.uxdroid.system.InputService` describes.
pub const KEYBOARD_DEVICE: i32 = 1;
pub const MOUSE_DEVICE: i32 = 2;
pub const TOUCH_DEVICE: i32 = 3;

/// `InputDevice.SOURCE_*`.
pub const SOURCE_KEYBOARD: i32 = 0x0000_0101;
pub const SOURCE_MOUSE: i32 = 0x0000_2002;
pub const SOURCE_TOUCHSCREEN: i32 = 0x0000_1002;

/// A key event, as `KeyEvent` holds it. Times are `CLOCK_MONOTONIC` nanoseconds, the clock of
/// `SystemClock.uptimeMillis`.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub down_time: i64,
    pub event_time: i64,
    pub action: i32,
    pub key_code: i32,
    pub scan_code: i32,
    pub meta_state: i32,
    pub repeat: i32,
    pub flags: i32,
    pub source: i32,
    pub device: i32,
}

/// One pointer of a motion event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    pub id: i32,
    pub tool: i32,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

/// A motion event, as `MotionEvent` holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pub down_time: i64,
    pub event_time: i64,
    pub action: i32,
    pub action_button: i32,
    pub button_state: i32,
    pub meta_state: i32,
    pub source: i32,
    pub device: i32,
    pub vscroll: f32,
    pub hscroll: f32,
    pub pointers: Vec<Pointer>,
}

/// What reaches a window's receiver.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Key(Key),
    Motion(Motion),
    /// The window gained or lost input focus, as `InputEventReceiver.onFocusEvent` reports.
    Focus(bool),
}

/// `CLOCK_MONOTONIC` in nanoseconds.
pub fn now() -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid timespec to write.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    time.tv_sec * 1_000_000_000 + time.tv_nsec
}
