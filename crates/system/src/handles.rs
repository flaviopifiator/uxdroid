//! Native objects handed to Java as numbers.
//!
//! The framework keeps a `long` for each native object and hands it back on every call, including
//! to the free functions `NativeAllocationRegistry` runs on its own thread. Handing out ids instead
//! of addresses means a stale, foreign or forged value finds nothing rather than memory.
//!
//! Each id is counted, as Android's native objects are: `retain` adds a reference to an id that
//! Java will release once more, such as the one `Surface.lockCanvas` returns.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// Shared by every registry, so an id of one kind is never valid as another.
static NEXT: AtomicI64 = AtomicI64::new(1);

pub struct Handles<T> {
    live: Mutex<BTreeMap<i64, (T, usize)>>,
}

impl<T: Clone> Handles<T> {
    pub const fn new() -> Self {
        Handles {
            live: Mutex::new(BTreeMap::new()),
        }
    }

    /// Never returns 0, which Java reads as "no object".
    pub fn add(&self, value: T) -> i64 {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(id, (value, 1));
        id
    }

    pub fn get(&self, id: i64) -> Option<T> {
        self.lock().get(&id).map(|(value, _)| value.clone())
    }

    /// Adds a reference to a live id; false if it is not live.
    pub fn retain(&self, id: i64) -> bool {
        match self.lock().get_mut(&id) {
            Some((_, count)) => {
                *count += 1;
                true
            }
            None => false,
        }
    }

    /// Drops one reference, and returns the value when it was the last. The value is dropped by
    /// the caller, after the registry is unlocked.
    pub fn remove(&self, id: i64) -> Option<T> {
        let mut live = self.lock();
        let (_, count) = live.get_mut(&id)?;
        *count -= 1;
        if *count > 0 {
            return None;
        }
        live.remove(&id).map(|(value, _)| value)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<i64, (T, usize)>> {
        // A panic while holding the lock cannot leave the map half-changed.
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_across_registries_and_die_once() {
        let numbers: Handles<u32> = Handles::new();
        let words: Handles<&str> = Handles::new();
        let one = numbers.add(1);
        let word = words.add("one");
        assert_ne!(one, 0);
        assert_ne!(one, word);
        assert_eq!(numbers.get(one), Some(1));
        assert_eq!(numbers.get(word), None);
        assert_eq!(numbers.remove(one), Some(1));
        assert_eq!(numbers.remove(one), None);
        assert_eq!(numbers.get(0), None);
    }

    #[test]
    fn a_retained_handle_lives_until_its_last_release() {
        let numbers: Handles<u32> = Handles::new();
        let one = numbers.add(1);
        assert!(numbers.retain(one));
        assert_eq!(numbers.remove(one), None);
        assert_eq!(numbers.get(one), Some(1));
        assert_eq!(numbers.remove(one), Some(1));
        assert_eq!(numbers.get(one), None);
        assert!(!numbers.retain(one));
    }
}
