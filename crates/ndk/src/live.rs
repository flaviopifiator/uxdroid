//! The objects NDK functions hand to app code, kept by address so that every pointer coming back is
//! checked: a stale, foreign or forged pointer is refused instead of being dereferenced.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

pub struct Live<T> {
    objects: Mutex<BTreeMap<usize, Arc<T>>>,
}

impl<T> Live<T> {
    pub const fn new() -> Self {
        Live {
            objects: Mutex::new(BTreeMap::new()),
        }
    }

    /// Keeps `object` alive and returns the opaque pointer app code will hold.
    pub fn add(&self, object: Arc<T>) -> *mut c_void {
        let address = Arc::as_ptr(&object) as usize;
        self.lock().insert(address, object);
        address as *mut c_void
    }

    pub fn get(&self, pointer: *const c_void) -> Option<Arc<T>> {
        self.lock().get(&(pointer as usize)).cloned()
    }

    pub fn remove(&self, pointer: *const c_void) -> Option<Arc<T>> {
        self.lock().remove(&(pointer as usize))
    }

    pub fn find(&self, matches: impl Fn(&T) -> bool) -> Option<*mut c_void> {
        self.lock()
            .iter()
            .find(|(_, object)| matches(object))
            .map(|(&address, _)| address as *mut c_void)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<usize, Arc<T>>> {
        self.objects.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_live_pointers_are_found() {
        let live = Live::new();
        let pointer = live.add(Arc::new(5));
        assert_eq!(live.get(pointer).as_deref(), Some(&5));
        assert!(live.get(pointer.wrapping_byte_add(1)).is_none());
        assert!(live.remove(pointer).is_some());
        assert!(live.get(pointer).is_none());
    }
}
