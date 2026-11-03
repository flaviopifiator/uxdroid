//! Wayland seat events to Android key and motion events, as Android's InputReader would make them
//! from the same devices.
//!
//! - The pointer is a mouse: hovering without buttons, `ACTION_DOWN` and `ACTION_BUTTON_PRESS` for
//!   the first button, `ACTION_SCROLL` for the wheel. The side buttons are the `BACK` and `FORWARD`
//!   keys, which InputReader synthesizes from a mouse's back and forward buttons.
//! - Touch points are fingers of one touchscreen, with Android pointer ids, the smallest free one
//!   for each new point. Moves are kept until the frame that ends them, and become one event.
//! - Keys come from the table in [`super::keys`], with the meta state of the last modifiers.

use super::keys::{self, ACTION_DOWN, ACTION_UP};
use super::{
    Event, KEYBOARD_DEVICE, Key, MOUSE_DEVICE, Motion, Pointer, SOURCE_KEYBOARD, SOURCE_MOUSE,
    SOURCE_TOUCHSCREEN, TOUCH_DEVICE, wire::MAX_POINTERS,
};

/// `MotionEvent.ACTION_*`.
pub const ACTION_MOVE: i32 = 2;
pub const ACTION_CANCEL: i32 = 3;
pub const ACTION_POINTER_DOWN: i32 = 5;
pub const ACTION_POINTER_UP: i32 = 6;
pub const ACTION_HOVER_MOVE: i32 = 7;
pub const ACTION_SCROLL: i32 = 8;
pub const ACTION_HOVER_ENTER: i32 = 9;
pub const ACTION_HOVER_EXIT: i32 = 10;
pub const ACTION_BUTTON_PRESS: i32 = 11;
pub const ACTION_BUTTON_RELEASE: i32 = 12;
pub const ACTION_POINTER_INDEX_SHIFT: i32 = 8;

/// `MotionEvent.BUTTON_*` and `TOOL_TYPE_*`.
pub const BUTTON_PRIMARY: i32 = 1;
pub const BUTTON_SECONDARY: i32 = 2;
pub const BUTTON_TERTIARY: i32 = 4;
pub const TOOL_TYPE_FINGER: i32 = 1;
pub const TOOL_TYPE_MOUSE: i32 = 3;

/// Linux `BTN_*` codes of `wl_pointer.button`.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
pub const BTN_SIDE: u32 = 0x113;
pub const BTN_EXTRA: u32 = 0x114;

#[derive(Clone, Copy)]
struct Touch {
    wayland: i32,
    id: i32,
    x: f32,
    y: f32,
}

/// The state of one seat, as one window sees it.
#[derive(Default)]
pub struct Translator {
    pointer: Option<(f32, f32)>,
    buttons: i32,
    button_down_time: i64,
    touches: Vec<Touch>,
    touch_down_time: i64,
    touch_moved: bool,
    meta: i32,
    keys_down: Vec<(u32, i64)>,
}

impl Translator {
    pub fn set_meta_state(&mut self, meta: i32) {
        self.meta = meta;
    }

    pub fn meta_state(&self) -> i32 {
        self.meta
    }

    fn mouse(&self, action: i32, action_button: i32, now: i64) -> Vec<Event> {
        let Some((x, y)) = self.pointer else {
            return Vec::new();
        };
        let pressed = self.buttons != 0 && !matches!(action, ACTION_UP);
        vec![Event::Motion(Motion {
            down_time: if self.buttons != 0 || matches!(action, ACTION_UP | ACTION_BUTTON_RELEASE) {
                self.button_down_time
            } else {
                now
            },
            event_time: now,
            action,
            action_button,
            button_state: self.buttons,
            meta_state: self.meta,
            source: SOURCE_MOUSE,
            device: MOUSE_DEVICE,
            vscroll: 0.0,
            hscroll: 0.0,
            pointers: vec![Pointer {
                id: 0,
                tool: TOOL_TYPE_MOUSE,
                x,
                y,
                pressure: if pressed { 1.0 } else { 0.0 },
            }],
        })]
    }

    pub fn pointer_enter(&mut self, x: f32, y: f32, now: i64) -> Vec<Event> {
        self.pointer = Some((x, y));
        self.buttons = 0;
        self.mouse(ACTION_HOVER_ENTER, 0, now)
    }

    pub fn pointer_leave(&mut self, now: i64) -> Vec<Event> {
        // A gesture the window will never see end is cancelled, as Android does for a lost window.
        let action = if self.buttons != 0 {
            ACTION_CANCEL
        } else {
            ACTION_HOVER_EXIT
        };
        let events = self.mouse(action, 0, now);
        self.pointer = None;
        self.buttons = 0;
        events
    }

    pub fn pointer_motion(&mut self, x: f32, y: f32, now: i64) -> Vec<Event> {
        if self.pointer.is_none() {
            return Vec::new();
        }
        self.pointer = Some((x, y));
        let action = if self.buttons != 0 {
            ACTION_MOVE
        } else {
            ACTION_HOVER_MOVE
        };
        self.mouse(action, 0, now)
    }

    pub fn pointer_button(&mut self, button: u32, pressed: bool, now: i64) -> Vec<Event> {
        if self.pointer.is_none() {
            return Vec::new();
        }
        let android = match button {
            BTN_LEFT => BUTTON_PRIMARY,
            BTN_RIGHT => BUTTON_SECONDARY,
            BTN_MIDDLE => BUTTON_TERTIARY,
            BTN_SIDE => return self.synthetic_key(button, keys::KEYCODE_BACK, pressed, now),
            BTN_EXTRA => return self.synthetic_key(button, keys::KEYCODE_FORWARD, pressed, now),
            _ => return Vec::new(),
        };
        let mut events = Vec::new();
        if pressed {
            if self.buttons & android != 0 {
                return events;
            }
            if self.buttons == 0 {
                self.button_down_time = now;
                self.buttons = android;
                events.extend(self.mouse(ACTION_DOWN, 0, now));
            } else {
                self.buttons |= android;
            }
            events.extend(self.mouse(ACTION_BUTTON_PRESS, android, now));
        } else {
            if self.buttons & android == 0 {
                return events;
            }
            self.buttons &= !android;
            events.extend(self.mouse(ACTION_BUTTON_RELEASE, android, now));
            if self.buttons == 0 {
                events.extend(self.mouse(ACTION_UP, 0, now));
            }
        }
        events
    }

    /// Scrolls by wheel steps; positive is down or right, as Wayland counts.
    pub fn pointer_scroll(&mut self, vertical: f32, horizontal: f32, now: i64) -> Vec<Event> {
        if vertical == 0.0 && horizontal == 0.0 {
            return Vec::new();
        }
        let mut events = self.mouse(ACTION_SCROLL, 0, now);
        if let Some(Event::Motion(motion)) = events.first_mut() {
            motion.vscroll = -vertical;
            motion.hscroll = horizontal;
        }
        events
    }

    /// Wheel steps at a point, as a mouse moved there and scrolled: hovers to the point, scrolls,
    /// and leaves again, or for a pointer already in the window, stays where it was.
    pub fn wheel_at(
        &mut self,
        x: f32,
        y: f32,
        vertical: f32,
        horizontal: f32,
        now: i64,
    ) -> Vec<Event> {
        if vertical == 0.0 && horizontal == 0.0 {
            return Vec::new();
        }
        let before = self.pointer.replace((x, y));
        let hover = if before.is_some() {
            ACTION_HOVER_MOVE
        } else {
            ACTION_HOVER_ENTER
        };
        let mut events = self.mouse(hover, 0, now);
        events.extend(self.pointer_scroll(vertical, horizontal, now));
        if before.is_none() {
            events.extend(self.mouse(ACTION_HOVER_EXIT, 0, now));
        }
        self.pointer = before;
        events
    }

    fn touch_event(&self, action: i32, now: i64) -> Vec<Event> {
        vec![Event::Motion(Motion {
            down_time: self.touch_down_time,
            event_time: now,
            action,
            action_button: 0,
            button_state: 0,
            meta_state: self.meta,
            source: SOURCE_TOUCHSCREEN,
            device: TOUCH_DEVICE,
            vscroll: 0.0,
            hscroll: 0.0,
            pointers: self
                .touches
                .iter()
                .map(|touch| Pointer {
                    id: touch.id,
                    tool: TOOL_TYPE_FINGER,
                    x: touch.x,
                    y: touch.y,
                    pressure: 1.0,
                })
                .collect(),
        })]
    }

    pub fn touch_down(&mut self, wayland: i32, x: f32, y: f32, now: i64) -> Vec<Event> {
        if self.touches.len() >= MAX_POINTERS || self.touches.iter().any(|t| t.wayland == wayland) {
            return Vec::new();
        }
        let id = (0..)
            .find(|id| self.touches.iter().all(|t| t.id != *id))
            .unwrap_or(0);
        self.touches.push(Touch { wayland, id, x, y });
        let action = if self.touches.len() == 1 {
            self.touch_down_time = now;
            self.touch_moved = false;
            ACTION_DOWN
        } else {
            ACTION_POINTER_DOWN | ((self.touches.len() as i32 - 1) << ACTION_POINTER_INDEX_SHIFT)
        };
        self.touch_event(action, now)
    }

    pub fn touch_motion(&mut self, wayland: i32, x: f32, y: f32) {
        if let Some(touch) = self.touches.iter_mut().find(|t| t.wayland == wayland) {
            (touch.x, touch.y) = (x, y);
            self.touch_moved = true;
        }
    }

    pub fn touch_up(&mut self, wayland: i32, now: i64) -> Vec<Event> {
        let Some(index) = self.touches.iter().position(|t| t.wayland == wayland) else {
            return Vec::new();
        };
        let action = if self.touches.len() == 1 {
            ACTION_UP
        } else {
            ACTION_POINTER_UP | ((index as i32) << ACTION_POINTER_INDEX_SHIFT)
        };
        let events = self.touch_event(action, now);
        self.touches.remove(index);
        events
    }

    pub fn touch_frame(&mut self, now: i64) -> Vec<Event> {
        if !std::mem::take(&mut self.touch_moved) || self.touches.is_empty() {
            return Vec::new();
        }
        self.touch_event(ACTION_MOVE, now)
    }

    pub fn touch_cancel(&mut self, now: i64) -> Vec<Event> {
        if self.touches.is_empty() {
            return Vec::new();
        }
        let events = self.touch_event(ACTION_CANCEL, now);
        self.touches.clear();
        self.touch_moved = false;
        events
    }

    fn key_event(
        &self,
        code: u32,
        key_code: i32,
        action: i32,
        down_time: i64,
        now: i64,
        source: i32,
    ) -> Event {
        Event::Key(Key {
            down_time,
            event_time: now,
            action,
            key_code,
            scan_code: code as i32,
            meta_state: self.meta,
            repeat: 0,
            flags: keys::FLAG_FROM_SYSTEM,
            source,
            device: if source == SOURCE_MOUSE {
                MOUSE_DEVICE
            } else {
                KEYBOARD_DEVICE
            },
        })
    }

    fn press(
        &mut self,
        code: u32,
        key_code: i32,
        pressed: bool,
        now: i64,
        source: i32,
    ) -> Vec<Event> {
        let held = self.keys_down.iter().position(|(down, _)| *down == code);
        match (pressed, held) {
            (true, None) => {
                self.keys_down.push((code, now));
                vec![self.key_event(code, key_code, ACTION_DOWN, now, now, source)]
            }
            (false, Some(index)) => {
                let (_, down_time) = self.keys_down.remove(index);
                vec![self.key_event(code, key_code, ACTION_UP, down_time, now, source)]
            }
            _ => Vec::new(),
        }
    }

    fn synthetic_key(&mut self, button: u32, key_code: i32, pressed: bool, now: i64) -> Vec<Event> {
        self.press(button, key_code, pressed, now, SOURCE_MOUSE)
    }

    /// A key of `wl_keyboard.key`: its evdev code, pressed or released.
    pub fn key(&mut self, evdev: u32, pressed: bool, now: i64) -> Vec<Event> {
        let Some(mapping) = keys::mapping(evdev) else {
            return Vec::new();
        };
        self.press(evdev, mapping.key_code, pressed, now, SOURCE_KEYBOARD)
    }

    /// Releases every key still held, cancelled, as Android does when a window loses focus.
    pub fn keyboard_leave(&mut self, now: i64) -> Vec<Event> {
        let held = std::mem::take(&mut self.keys_down);
        held.into_iter()
            .filter_map(|(code, down_time)| {
                let (key_code, source) = match code {
                    BTN_SIDE => (keys::KEYCODE_BACK, SOURCE_MOUSE),
                    BTN_EXTRA => (keys::KEYCODE_FORWARD, SOURCE_MOUSE),
                    _ => (keys::mapping(code)?.key_code, SOURCE_KEYBOARD),
                };
                let mut event = self.key_event(code, key_code, ACTION_UP, down_time, now, source);
                if let Event::Key(key) = &mut event {
                    key.flags |= FLAG_CANCELED;
                }
                Some(event)
            })
            .collect()
    }
}

/// `KeyEvent.FLAG_CANCELED`.
pub const FLAG_CANCELED: i32 = 0x20;

#[cfg(test)]
mod tests {
    use super::*;

    fn motions(events: &[Event]) -> Vec<&Motion> {
        events
            .iter()
            .map(|event| match event {
                Event::Motion(motion) => motion,
                other => panic!("not a motion: {other:?}"),
            })
            .collect()
    }

    fn keys(events: &[Event]) -> Vec<&Key> {
        events
            .iter()
            .map(|event| match event {
                Event::Key(key) => key,
                other => panic!("not a key: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_mouse_click_is_down_press_release_up_where_the_pointer_is() {
        let mut seat = Translator::default();
        let enter = seat.pointer_enter(10.0, 20.0, 1);
        let hover = motions(&enter);
        assert_eq!(hover.len(), 1);
        assert_eq!(hover[0].action, ACTION_HOVER_ENTER);
        assert_eq!(hover[0].source, SOURCE_MOUSE);
        assert_eq!(hover[0].device, MOUSE_DEVICE);
        assert_eq!(
            hover[0].pointers,
            vec![Pointer {
                id: 0,
                tool: TOOL_TYPE_MOUSE,
                x: 10.0,
                y: 20.0,
                pressure: 0.0
            }]
        );
        assert_eq!(
            motions(&seat.pointer_motion(30.5, 40.0, 2))[0].action,
            ACTION_HOVER_MOVE
        );

        let press = seat.pointer_button(BTN_LEFT, true, 3);
        let press = motions(&press);
        assert_eq!(
            press.iter().map(|m| m.action).collect::<Vec<_>>(),
            [ACTION_DOWN, ACTION_BUTTON_PRESS]
        );
        assert!(press.iter().all(|m| m.button_state == BUTTON_PRIMARY
            && m.down_time == 3
            && m.pointers[0].x == 30.5
            && m.pointers[0].pressure == 1.0));
        assert_eq!(press[1].action_button, BUTTON_PRIMARY);

        let drag = seat.pointer_motion(31.0, 41.0, 4);
        assert_eq!(motions(&drag)[0].action, ACTION_MOVE);
        assert_eq!(motions(&drag)[0].down_time, 3);

        let release = seat.pointer_button(BTN_LEFT, false, 5);
        let release = motions(&release);
        assert_eq!(
            release.iter().map(|m| m.action).collect::<Vec<_>>(),
            [ACTION_BUTTON_RELEASE, ACTION_UP]
        );
        assert_eq!(release[0].action_button, BUTTON_PRIMARY);
        assert_eq!(release[1].button_state, 0);
        assert_eq!(release[1].event_time, 5);
        assert_eq!(
            motions(&seat.pointer_motion(32.0, 42.0, 6))[0].action,
            ACTION_HOVER_MOVE
        );
        assert_eq!(motions(&seat.pointer_leave(7))[0].action, ACTION_HOVER_EXIT);
    }

    #[test]
    fn a_second_button_only_adds_a_press() {
        let mut seat = Translator::default();
        seat.pointer_enter(1.0, 1.0, 0);
        seat.pointer_button(BTN_LEFT, true, 1);
        let second = seat.pointer_button(BTN_RIGHT, true, 2);
        let second = motions(&second);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].action, ACTION_BUTTON_PRESS);
        assert_eq!(second[0].action_button, BUTTON_SECONDARY);
        assert_eq!(second[0].button_state, BUTTON_PRIMARY | BUTTON_SECONDARY);
        let middle_up = seat.pointer_button(BTN_MIDDLE, false, 3);
        assert!(
            middle_up.is_empty(),
            "a button that is not down releases nothing"
        );
    }

    #[test]
    fn pointer_events_before_enter_go_nowhere() {
        let mut seat = Translator::default();
        assert!(seat.pointer_motion(1.0, 1.0, 0).is_empty());
        assert!(seat.pointer_button(BTN_LEFT, true, 0).is_empty());
        assert!(seat.pointer_scroll(1.0, 0.0, 0).is_empty());
        assert!(seat.pointer_leave(0).is_empty());
    }

    #[test]
    fn leaving_with_a_button_down_cancels_the_gesture() {
        let mut seat = Translator::default();
        seat.pointer_enter(1.0, 1.0, 0);
        seat.pointer_button(BTN_LEFT, true, 1);
        let leave = seat.pointer_leave(2);
        assert_eq!(motions(&leave)[0].action, ACTION_CANCEL);
        assert!(seat.pointer_button(BTN_LEFT, false, 3).is_empty());
    }

    #[test]
    fn a_wheel_at_a_point_hovers_there_and_leaves_the_pointer_as_it_was() {
        let mut seat = Translator::default();
        let events = seat.wheel_at(30.0, 40.0, 3.0, 0.0, 5);
        let events = motions(&events);
        let actions: Vec<i32> = events.iter().map(|m| m.action).collect();
        assert_eq!(
            actions,
            [ACTION_HOVER_ENTER, ACTION_SCROLL, ACTION_HOVER_EXIT]
        );
        assert!(
            events
                .iter()
                .all(|m| m.pointers[0].x == 30.0 && m.pointers[0].y == 40.0)
        );
        assert_eq!(events[1].vscroll, -3.0);
        // No pointer before, none after: motion without an enter is still ignored.
        assert!(seat.pointer_motion(1.0, 1.0, 6).is_empty());
        // A pointer that was in the window stays where it was.
        seat.pointer_enter(5.0, 6.0, 7);
        let events = seat.wheel_at(30.0, 40.0, 0.0, 1.0, 8);
        let actions: Vec<i32> = motions(&events).iter().map(|m| m.action).collect();
        assert_eq!(actions, [ACTION_HOVER_MOVE, ACTION_SCROLL]);
        let moved = seat.pointer_motion(5.0, 6.0, 9);
        assert_eq!(motions(&moved)[0].pointers[0].x, 5.0);
        assert!(seat.wheel_at(1.0, 1.0, 0.0, 0.0, 10).is_empty());
    }

    #[test]
    fn the_wheel_scrolls_with_android_signs() {
        let mut seat = Translator::default();
        seat.pointer_enter(5.0, 6.0, 0);
        let scroll = seat.pointer_scroll(2.0, -1.0, 1);
        let scroll = motions(&scroll);
        assert_eq!(scroll[0].action, ACTION_SCROLL);
        // Wayland counts down as positive; Android's AXIS_VSCROLL counts up.
        assert_eq!((scroll[0].vscroll, scroll[0].hscroll), (-2.0, -1.0));
        assert!(seat.pointer_scroll(0.0, 0.0, 2).is_empty());
    }

    #[test]
    fn side_buttons_are_back_and_forward_keys() {
        let mut seat = Translator::default();
        seat.pointer_enter(1.0, 1.0, 0);
        let back = seat.pointer_button(BTN_SIDE, true, 1);
        let back = keys(&back);
        assert_eq!(
            (back[0].action, back[0].key_code, back[0].source),
            (ACTION_DOWN, keys::KEYCODE_BACK, SOURCE_MOUSE)
        );
        let up = seat.pointer_button(BTN_SIDE, false, 2);
        assert_eq!(keys(&up)[0].action, ACTION_UP);
        assert_eq!(keys(&up)[0].down_time, 1);
        let forward = seat.pointer_button(BTN_EXTRA, true, 3);
        assert_eq!(keys(&forward)[0].key_code, keys::KEYCODE_FORWARD);
    }

    #[test]
    fn touch_points_become_pointers_of_one_gesture() {
        let mut seat = Translator::default();
        let down = seat.touch_down(17, 10.0, 20.0, 100);
        let down = motions(&down);
        assert_eq!(down[0].action, ACTION_DOWN);
        assert_eq!(down[0].source, SOURCE_TOUCHSCREEN);
        assert_eq!(down[0].device, TOUCH_DEVICE);
        assert_eq!(down[0].pointers[0].id, 0);
        assert_eq!(down[0].pointers[0].tool, TOOL_TYPE_FINGER);

        let second = seat.touch_down(-4, 50.0, 60.0, 110);
        let second = motions(&second);
        assert_eq!(
            second[0].action,
            ACTION_POINTER_DOWN | 1 << ACTION_POINTER_INDEX_SHIFT
        );
        assert_eq!(second[0].pointers.len(), 2);
        assert_eq!(second[0].pointers[1].id, 1);
        assert_eq!(second[0].down_time, 100);

        seat.touch_motion(17, 11.0, 21.0);
        seat.touch_motion(-4, 51.0, 61.0);
        seat.touch_motion(99, 0.0, 0.0);
        let frame = seat.touch_frame(120);
        let frame = motions(&frame);
        assert_eq!(frame.len(), 1, "moves in one frame become one event");
        assert_eq!(frame[0].action, ACTION_MOVE);
        assert_eq!(
            (frame[0].pointers[0].x, frame[0].pointers[1].y),
            (11.0, 61.0)
        );
        assert!(seat.touch_frame(121).is_empty(), "no move, no event");

        let first_up = seat.touch_up(17, 130);
        let first_up = motions(&first_up);
        assert_eq!(first_up[0].action, ACTION_POINTER_UP);
        assert_eq!(first_up[0].pointers.len(), 2);
        // The freed id goes to the next new point.
        let third = seat.touch_down(5, 1.0, 1.0, 135);
        assert_eq!(motions(&third)[0].pointers[1].id, 0);
        seat.touch_up(5, 136);
        let last_up = seat.touch_up(-4, 140);
        let last_up = motions(&last_up);
        assert_eq!(last_up[0].action, ACTION_UP);
        assert_eq!(last_up[0].pointers.len(), 1);
        assert!(
            seat.touch_up(-4, 141).is_empty(),
            "an unknown point ends nothing"
        );
    }

    #[test]
    fn a_cancel_ends_every_touch_point() {
        let mut seat = Translator::default();
        seat.touch_down(1, 1.0, 1.0, 0);
        seat.touch_down(2, 2.0, 2.0, 1);
        let cancel = seat.touch_cancel(2);
        let cancel = motions(&cancel);
        assert_eq!(cancel[0].action, ACTION_CANCEL);
        assert_eq!(cancel[0].pointers.len(), 2);
        assert!(seat.touch_cancel(3).is_empty());
        assert_eq!(
            motions(&seat.touch_down(3, 0.0, 0.0, 4))[0].action,
            ACTION_DOWN
        );
    }

    #[test]
    fn touch_points_past_android_s_limit_are_ignored() {
        let mut seat = Translator::default();
        for point in 0..MAX_POINTERS as i32 {
            assert_eq!(seat.touch_down(point, 0.0, 0.0, 0).len(), 1);
        }
        assert!(seat.touch_down(1000, 0.0, 0.0, 0).is_empty());
        assert!(
            seat.touch_down(0, 0.0, 0.0, 0).is_empty(),
            "a point already down"
        );
    }

    #[test]
    fn keys_carry_their_code_scan_code_meta_and_down_time() {
        let mut seat = Translator::default();
        seat.set_meta_state(keys::META_SHIFT_ON | keys::META_SHIFT_LEFT_ON);
        let down = seat.key(30, true, 10);
        let down = keys(&down);
        assert_eq!(down.len(), 1);
        assert_eq!(
            (down[0].action, down[0].key_code, down[0].scan_code),
            (ACTION_DOWN, 29, 30)
        );
        assert_eq!(
            down[0].meta_state,
            keys::META_SHIFT_ON | keys::META_SHIFT_LEFT_ON
        );
        assert_eq!(
            (down[0].source, down[0].device, down[0].down_time),
            (SOURCE_KEYBOARD, KEYBOARD_DEVICE, 10)
        );
        let up = seat.key(30, false, 20);
        assert_eq!(
            (
                keys(&up)[0].action,
                keys(&up)[0].down_time,
                keys(&up)[0].event_time
            ),
            (ACTION_UP, 10, 20)
        );
        assert!(seat.key(30, false, 21).is_empty(), "a key that is not down");
        assert!(
            seat.key(0x2ff, true, 22).is_empty(),
            "a key Android has no code for"
        );
    }

    #[test]
    fn losing_the_keyboard_releases_held_keys() {
        let mut seat = Translator::default();
        seat.key(30, true, 1);
        seat.key(31, true, 2);
        let released = seat.keyboard_leave(3);
        let released = keys(&released);
        assert_eq!(released.len(), 2);
        assert!(
            released
                .iter()
                .all(|k| k.action == ACTION_UP && k.flags & 0x20 != 0 && k.event_time == 3)
        );
        assert!(seat.keyboard_leave(4).is_empty());
    }
}
