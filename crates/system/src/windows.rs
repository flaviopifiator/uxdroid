//! The windows of one display and where their input goes: InputDispatcher's part of the window
//! manager, over the stack of an app's windows.
//!
//! All of an app's windows share one Wayland toplevel, which is the display: the first window is
//! the toplevel's surface and the others, dialogs, popups and menus, are subsurfaces placed where
//! the window manager put them. Wayland reports pointer and touch events on the surface under them,
//! in that surface's coordinates; this module moves them to display coordinates and gives them to
//! the window Android would: the top window under the point, or a touch-modal window above it,
//! which takes every touch, and a window that watches outside touches gets `ACTION_OUTSIDE` when a
//! gesture starts outside it, below it. A gesture stays with the window it started in. Keys and
//! focus go to the top focusable window, or the bottom one when none is.

use crate::input::{Event, Motion};

/// `WindowManager.LayoutParams` flags read here.
pub const FLAG_NOT_FOCUSABLE: i32 = 0x0000_0008;
pub const FLAG_NOT_TOUCH_MODAL: i32 = 0x0000_0020;
pub const FLAG_WATCH_OUTSIDE_TOUCH: i32 = 0x0004_0000;

const ACTION_MASK: i32 = 0xff;
const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_CANCEL: i32 = 3;
const ACTION_OUTSIDE: i32 = 4;

/// Where a window is on the display, and how it takes input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The window's frame, which its own coordinates start from and touches hit.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// `WindowManager.LayoutParams.flags`.
    pub flags: i32,
    /// How far the window's surface starts before its frame: `LayoutParams.surfaceInsets`, room
    /// for a shadow.
    pub inset_left: i32,
    pub inset_top: i32,
}

impl Placement {
    /// Where the window's surface goes on the display.
    pub fn surface(&self) -> (i32, i32) {
        (
            self.x.saturating_sub(self.inset_left),
            self.y.saturating_sub(self.inset_top),
        )
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && y >= self.y as f32
            && x < (self.x as i64 + self.width as i64) as f32
            && y < (self.y as i64 + self.height as i64) as f32
    }

    fn focusable(&self) -> bool {
        self.flags & FLAG_NOT_FOCUSABLE == 0
    }

    /// A focusable window without `FLAG_NOT_TOUCH_MODAL` takes every touch, as on Android.
    fn touch_modal(&self) -> bool {
        self.flags & (FLAG_NOT_FOCUSABLE | FLAG_NOT_TOUCH_MODAL) == 0
    }
}

/// Where an event came from: a window's surface, in its coordinates, or the keyboard and focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source<K> {
    Surface(K),
    Focused,
}

/// The windows, bottom first.
pub struct Stack<K> {
    windows: Vec<(K, Placement)>,
    activated: bool,
    focused: Option<K>,
    /// The window holding the current gesture, if one is down.
    gesture: Option<K>,
}

impl<K> Stack<K> {
    pub const fn new() -> Self {
        Stack {
            windows: Vec::new(),
            activated: false,
            focused: None,
            gesture: None,
        }
    }
}

impl<K> Default for Stack<K> {
    fn default() -> Self {
        Stack::new()
    }
}

fn moved(motion: &Motion, dx: f32, dy: f32) -> Motion {
    let mut motion = motion.clone();
    for pointer in &mut motion.pointers {
        pointer.x += dx;
        pointer.y += dy;
    }
    motion
}

impl<K: Copy + PartialEq> Stack<K> {
    pub fn placement(&self, key: K) -> Option<Placement> {
        self.windows
            .iter()
            .find(|(window, _)| *window == key)
            .map(|(_, placement)| *placement)
    }

    /// Adds a window on top, or moves one that is there; returns the focus changes.
    pub fn place(&mut self, key: K, placement: Placement) -> Vec<(K, Event)> {
        match self.windows.iter_mut().find(|(window, _)| *window == key) {
            Some((_, known)) => *known = placement,
            None => self.windows.push((key, placement)),
        }
        self.refocus()
    }

    pub fn remove(&mut self, key: K) -> Vec<(K, Event)> {
        self.windows.retain(|(window, _)| *window != key);
        if self.gesture == Some(key) {
            self.gesture = None;
        }
        if self.focused == Some(key) {
            self.focused = None;
        }
        self.refocus()
    }

    fn focus_target(&self) -> Option<K> {
        self.windows
            .iter()
            .rev()
            .find(|(_, placement)| placement.focusable())
            .or(self.windows.first())
            .map(|(key, _)| *key)
    }

    fn refocus(&mut self) -> Vec<(K, Event)> {
        let target = self.focus_target();
        if target == self.focused {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.activated {
            if let Some(old) = self.focused {
                events.push((old, Event::Focus(false)));
            }
            if let Some(new) = target {
                events.push((new, Event::Focus(true)));
            }
        }
        self.focused = target;
        events
    }

    /// The windows that get `event`, each in its own coordinates.
    pub fn route(&mut self, source: Source<K>, event: Event) -> Vec<(K, Event)> {
        match (source, event) {
            (_, Event::Focus(activated)) => {
                self.activated = activated;
                self.focused = self.focus_target();
                self.focused
                    .map(|key| (key, Event::Focus(activated)))
                    .into_iter()
                    .collect()
            }
            (Source::Focused, Event::Key(key)) => self
                .focus_target()
                .map(|window| (window, Event::Key(key)))
                .into_iter()
                .collect(),
            (Source::Surface(surface), Event::Key(key)) => vec![(surface, Event::Key(key))],
            (Source::Focused, Event::Motion(motion)) => self
                .focus_target()
                .map(|window| (window, Event::Motion(motion)))
                .into_iter()
                .collect(),
            (Source::Surface(surface), Event::Motion(motion)) => self.route_motion(surface, motion),
        }
    }

    fn route_motion(&mut self, surface: K, motion: Motion) -> Vec<(K, Event)> {
        let Some(from) = self.placement(surface) else {
            return vec![(surface, Event::Motion(motion))];
        };
        // Wayland's coordinates are the surface's; the display's start at its origin.
        let (dx, dy) = from.surface();
        let (dx, dy) = (dx as f32, dy as f32);
        let point = motion
            .pointers
            .first()
            .map(|pointer| (pointer.x + dx, pointer.y + dy));
        let action = motion.action & ACTION_MASK;
        let mut events = Vec::new();
        let target = match (self.gesture, action) {
            (Some(window), _) if action != ACTION_DOWN && self.placement(window).is_some() => {
                Some(window)
            }
            _ => {
                let Some((x, y)) = point else {
                    return vec![(surface, Event::Motion(motion))];
                };
                let mut found = None;
                for (key, placement) in self.windows.iter().rev() {
                    if placement.contains(x, y) || placement.touch_modal() {
                        found = Some(*key);
                        break;
                    }
                    if action == ACTION_DOWN && placement.flags & FLAG_WATCH_OUTSIDE_TOUCH != 0 {
                        let mut outside =
                            moved(&motion, dx - placement.x as f32, dy - placement.y as f32);
                        outside.action = ACTION_OUTSIDE;
                        events.push((*key, Event::Motion(outside)));
                    }
                }
                found
            }
        };
        let Some(target) = target else {
            return events;
        };
        if action == ACTION_DOWN {
            self.gesture = Some(target);
        } else if action == ACTION_UP || action == ACTION_CANCEL {
            self.gesture = None;
        }
        let to = self.placement(target).unwrap_or(from);
        events.push((
            target,
            Event::Motion(moved(&motion, dx - to.x as f32, dy - to.y as f32)),
        ));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, Pointer};

    fn window(x: i32, y: i32, width: i32, height: i32, flags: i32) -> Placement {
        Placement {
            x,
            y,
            width,
            height,
            flags,
            inset_left: 0,
            inset_top: 0,
        }
    }

    fn touch(action: i32, x: f32, y: f32) -> Event {
        Event::Motion(Motion {
            down_time: 0,
            event_time: 0,
            action,
            action_button: 0,
            button_state: 0,
            meta_state: 0,
            source: 0,
            device: 0,
            vscroll: 0.0,
            hscroll: 0.0,
            pointers: vec![Pointer {
                id: 0,
                tool: 1,
                x,
                y,
                pressure: 1.0,
            }],
        })
    }

    fn key() -> Event {
        Event::Key(Key {
            down_time: 0,
            event_time: 0,
            action: 0,
            key_code: 29,
            scan_code: 30,
            meta_state: 0,
            repeat: 0,
            flags: 0,
            source: 0,
            device: 0,
        })
    }

    fn at(events: &[(u32, Event)]) -> Vec<(u32, i32, f32, f32)> {
        events
            .iter()
            .filter_map(|(key, event)| match event {
                Event::Motion(motion) => Some((
                    *key,
                    motion.action,
                    motion.pointers[0].x,
                    motion.pointers[0].y,
                )),
                _ => None,
            })
            .collect()
    }

    const ACTIVITY: u32 = 1;
    const POPUP: u32 = 2;
    const DIALOG: u32 = 3;

    #[test]
    fn a_touch_on_a_popup_reaches_it_in_its_own_coordinates() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        stack.place(POPUP, window(100, 200, 50, 50, FLAG_NOT_FOCUSABLE));

        let events = stack.route(Source::Surface(POPUP), touch(ACTION_DOWN, 10.0, 20.0));

        assert_eq!(at(&events), [(POPUP, ACTION_DOWN, 10.0, 20.0)]);
        // Under the popup's surface but outside it, the activity gets it, in its coordinates.
        let events = stack.route(Source::Surface(ACTIVITY), touch(ACTION_DOWN, 300.0, 300.0));
        assert_eq!(at(&events), [(ACTIVITY, ACTION_DOWN, 300.0, 300.0)]);
    }

    #[test]
    fn a_touch_modal_window_takes_touches_outside_it() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        stack.place(DIALOG, window(50, 300, 300, 100, 0));

        let events = stack.route(Source::Surface(ACTIVITY), touch(ACTION_DOWN, 10.0, 10.0));

        // Outside the dialog, in its coordinates, as Dialog's cancel-on-touch-outside reads it.
        assert_eq!(at(&events), [(DIALOG, ACTION_DOWN, -40.0, -290.0)]);
    }

    #[test]
    fn a_window_watching_outside_touches_gets_action_outside() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        let flags = FLAG_NOT_FOCUSABLE | FLAG_WATCH_OUTSIDE_TOUCH;
        stack.place(POPUP, window(100, 200, 50, 50, flags));

        let events = stack.route(Source::Surface(ACTIVITY), touch(ACTION_DOWN, 10.0, 10.0));

        assert_eq!(
            at(&events),
            [
                (POPUP, ACTION_OUTSIDE, -90.0, -190.0),
                (ACTIVITY, ACTION_DOWN, 10.0, 10.0)
            ]
        );
        // Only the gesture's start is reported outside.
        let events = stack.route(Source::Surface(ACTIVITY), touch(2, 20.0, 20.0));
        assert_eq!(at(&events), [(ACTIVITY, 2, 20.0, 20.0)]);
    }

    #[test]
    fn a_gesture_stays_with_the_window_it_started_in() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        stack.place(POPUP, window(100, 200, 50, 50, FLAG_NOT_FOCUSABLE));
        stack.route(Source::Surface(POPUP), touch(ACTION_DOWN, 10.0, 10.0));

        // Dragged off the popup onto the activity's surface.
        let events = stack.route(Source::Surface(ACTIVITY), touch(2, 300.0, 300.0));
        assert_eq!(at(&events), [(POPUP, 2, 200.0, 100.0)]);
        let events = stack.route(Source::Surface(ACTIVITY), touch(ACTION_UP, 300.0, 300.0));
        assert_eq!(at(&events), [(POPUP, ACTION_UP, 200.0, 100.0)]);
        let events = stack.route(Source::Surface(ACTIVITY), touch(ACTION_DOWN, 300.0, 300.0));
        assert_eq!(at(&events), [(ACTIVITY, ACTION_DOWN, 300.0, 300.0)]);
    }

    #[test]
    fn keys_and_focus_go_to_the_top_focusable_window() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        assert_eq!(
            stack.route(Source::Focused, Event::Focus(true)),
            [(ACTIVITY, Event::Focus(true))]
        );
        // A popup that takes no focus leaves keys with the activity.
        stack.place(POPUP, window(0, 0, 10, 10, FLAG_NOT_FOCUSABLE));
        assert_eq!(stack.route(Source::Focused, key()), [(ACTIVITY, key())]);

        // A dialog takes the focus, and gives it back when it goes.
        let moved = stack.place(DIALOG, window(0, 0, 10, 10, 0));
        assert_eq!(
            moved,
            [
                (ACTIVITY, Event::Focus(false)),
                (DIALOG, Event::Focus(true))
            ]
        );
        assert_eq!(stack.route(Source::Focused, key()), [(DIALOG, key())]);
        // A removed window has no channel left to tell.
        let moved = stack.remove(DIALOG);
        assert_eq!(moved, [(ACTIVITY, Event::Focus(true))]);
    }

    #[test]
    fn no_focus_changes_are_reported_while_the_toplevel_is_inactive() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        assert!(stack.place(DIALOG, window(0, 0, 10, 10, 0)).is_empty());
        assert_eq!(
            stack.route(Source::Focused, Event::Focus(true)),
            [(DIALOG, Event::Focus(true))]
        );
    }

    #[test]
    fn a_surface_with_insets_reports_touches_in_its_frame() {
        let mut stack = Stack::default();
        stack.place(ACTIVITY, window(0, 0, 400, 700, 0));
        let mut dialog = window(10, 255, 380, 190, 0);
        (dialog.inset_left, dialog.inset_top) = (32, 32);
        stack.place(DIALOG, dialog);
        assert_eq!(dialog.surface(), (-22, 223));

        // 40,40 in the surface is 8,8 in the frame, past the shadow.
        let events = stack.route(Source::Surface(DIALOG), touch(ACTION_DOWN, 40.0, 40.0));
        assert_eq!(at(&events), [(DIALOG, ACTION_DOWN, 8.0, 8.0)]);
    }

    #[test]
    fn events_of_an_unknown_surface_reach_it_unchanged() {
        let mut stack: Stack<u32> = Stack::default();
        let events = stack.route(Source::Surface(9), touch(ACTION_DOWN, 1.0, 2.0));
        assert_eq!(at(&events), [(9, ACTION_DOWN, 1.0, 2.0)]);
    }
}
