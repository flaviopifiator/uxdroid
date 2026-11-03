//! Android system properties, held in this process and read from the runtime's `build.prop`.

use std::sync::RwLock;

/// Keys in the order they were first set, so a handle from `find` stays valid.
pub struct Properties {
    entries: RwLock<Vec<(String, String)>>,
}

impl Default for Properties {
    fn default() -> Self {
        Self::new()
    }
}

impl Properties {
    pub const fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Loads `key=value` lines, skipping comments and lines with no `=`.
    pub fn load(&self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let _ = self.set(key.trim(), value.trim());
            }
        }
    }

    /// Like Android, a `ro.` property cannot change once it has a value.
    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        if key.is_empty() {
            return Err("empty property name".into());
        }
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        match entries.iter_mut().find(|(k, _)| k == key) {
            Some((_, old)) if key.starts_with("ro.") && !old.is_empty() => {
                Err(format!("{key} is read-only"))
            }
            Some((_, old)) => {
                *old = value.to_string();
                Ok(())
            }
            None => {
                entries.push((key.to_string(), value.to_string()));
                Ok(())
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// A handle for `key`, or 0 when it has never been set.
    pub fn find(&self, key: &str) -> i64 {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries
            .iter()
            .position(|(k, _)| k == key)
            .map_or(0, |index| index as i64 + 1)
    }

    pub fn get_handle(&self, handle: i64) -> Option<String> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        usize::try_from(handle - 1)
            .ok()
            .and_then(|index| entries.get(index))
            .map(|(_, v)| v.clone())
    }
}

/// Integers as Android parses them: decimal, or hexadecimal after `0x`, with an optional sign.
pub fn parse_int(value: Option<&str>) -> Option<i64> {
    let value = value?.trim();
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let magnitude = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<i64>().ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

pub fn parse_bool(value: Option<&str>) -> Option<bool> {
    match value? {
        "1" | "y" | "yes" | "on" | "true" => Some(true),
        "0" | "n" | "no" | "off" | "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_build_prop_and_keeps_read_only_values() {
        let props = Properties::new();
        props
            .load("# comment\nro.build.version.sdk=36\nro.empty=\npersist.x = 1\nnot a property\n");
        assert_eq!(props.get("ro.build.version.sdk").as_deref(), Some("36"));
        assert_eq!(props.get("persist.x").as_deref(), Some("1"));
        assert!(props.set("ro.build.version.sdk", "1").is_err());
        assert_eq!(props.get("ro.build.version.sdk").as_deref(), Some("36"));
        // An empty read-only property can still get its first value.
        assert!(props.set("ro.empty", "x86_64").is_ok());
        assert!(props.set("persist.x", "2").is_ok());
        assert_eq!(props.get("persist.x").as_deref(), Some("2"));
    }

    #[test]
    fn handles_follow_their_key() {
        let props = Properties::new();
        assert_eq!(props.find("debug.a"), 0);
        props.set("debug.a", "1").unwrap();
        let handle = props.find("debug.a");
        props.set("debug.a", "2").unwrap();
        assert_eq!(props.get_handle(handle).as_deref(), Some("2"));
        assert_eq!(props.get_handle(0), None);
        assert_eq!(props.get_handle(99), None);
    }

    #[test]
    fn parses_numbers_and_booleans_like_android() {
        assert_eq!(parse_int(Some("42")), Some(42));
        assert_eq!(parse_int(Some("-0x10")), Some(-16));
        assert_eq!(parse_int(Some("4x")), None);
        assert_eq!(parse_int(None), None);
        assert_eq!(parse_bool(Some("on")), Some(true));
        assert_eq!(parse_bool(Some("false")), Some(false));
        assert_eq!(parse_bool(Some("maybe")), None);
    }
}
