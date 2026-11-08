//! The desktop's application menu: an app installed into the default data directory, the one
//! `uxdroid app <package> start` starts from, gets `$XDG_DATA_HOME/applications/uxdroid-<package>.desktop`,
//! with its label, its launcher icon in `$XDG_DATA_HOME/uxdroid/icons`, and a command that starts it.
//! Apps of other data directories get none: one menu entry per package cannot tell them apart.
//!
//! Labels and icons come from the APK, so they are untrusted: a label loses its control characters
//! and is escaped, and an icon is written under a name of its package, which installs validate.

use std::path::{Path, PathBuf};

use uxdroid_apk::{Appearance, ImageFormat};
use uxdroid_store::write_atomically;

use crate::runtime::home_dir;

/// Names longer than this are cut: a menu shows a few words, and a crafted label can be megabytes.
const MAX_NAME_CHARS: usize = 128;
const FORMATS: [ImageFormat; 2] = [ImageFormat::Png, ImageFormat::Webp];

/// Where the menu entries of the default data directory's apps go.
pub(crate) struct Menu {
    applications: PathBuf,
    icons: PathBuf,
}

impl Menu {
    /// The menu of `data` when it is the default data directory, `None` for any other.
    pub(crate) fn of(data: &Path) -> Option<Menu> {
        let share = home_dir("XDG_DATA_HOME", ".local/share").ok()?;
        // The XDG specification ignores a relative directory.
        if !share.is_absolute() {
            return None;
        }
        let default = share.join("uxdroid/data");
        let same = data == default
            || matches!(
                (std::fs::canonicalize(data), std::fs::canonicalize(&default)),
                (Ok(a), Ok(b)) if a == b
            );
        same.then(|| Menu {
            applications: share.join("applications"),
            icons: share.join("uxdroid/icons"),
        })
    }

    fn entry(&self, package: &str) -> PathBuf {
        self.applications.join(format!("uxdroid-{package}.desktop"))
    }

    fn icon(&self, package: &str, format: ImageFormat) -> PathBuf {
        self.icons.join(format!("{package}.{}", format.extension()))
    }

    /// Writes the entry of `package`, started by `launcher`, or removes it when the app has no
    /// launcher Activity any more.
    pub(crate) fn update(
        &self,
        package: &str,
        launcher: Option<&str>,
        look: &Appearance,
    ) -> Result<(), String> {
        if launcher.is_none() {
            return self.remove(package);
        }
        let entry = self.entry(package);
        owned(package, &entry)?;
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|error| format!("cannot locate this program: {error}"))?;
        let exe = exe
            .to_str()
            .filter(|text| !text.chars().any(char::is_control))
            .ok_or_else(|| {
                format!(
                    "{} holds a control character or is not UTF-8, which a menu entry cannot hold",
                    exe.display()
                )
            })?;
        let icon = match &look.icon {
            Some(icon) => {
                let path = self.icon(package, icon.format);
                write_atomically(&path, &icon.bytes).map_err(|error| error.0)?;
                self.remove_icons(package, Some(icon.format))?;
                path.to_str()
                    .filter(|text| !text.chars().any(char::is_control))
                    .map(str::to_string)
            }
            None => {
                self.remove_icons(package, None)?;
                None
            }
        };
        let mut text = String::from("[Desktop Entry]\nType=Application\n");
        let name = look
            .label
            .as_deref()
            .map(clean)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| package.to_string());
        text.push_str(&format!("Name={}\n", escape(&name)));
        for (locale, label) in &look.labels {
            let label = clean(label);
            if !label.is_empty() && label != name {
                text.push_str(&format!("Name[{locale}]={}\n", escape(&label)));
            }
        }
        text.push_str(&format!(
            "Exec={} app {package} start\n",
            escape(&exec_word(exe))
        ));
        if let Some(icon) = icon {
            text.push_str(&format!("Icon={}\n", escape(&icon)));
        }
        // Windows of the app have the Wayland app id `uxdroid-<package>`, this entry's basename, which
        // shells match by itself; KDE and older shells also read this key.
        text.push_str(&format!("StartupWMClass=uxdroid-{package}\n"));
        text.push_str(&format!("Terminal=false\nX-Uxdroid-Package={package}\n"));
        write_atomically(&entry, text.as_bytes()).map_err(|error| error.0)
    }

    /// Removes the entry and icon of `package`, if uxdroid wrote them.
    pub(crate) fn remove(&self, package: &str) -> Result<(), String> {
        let entry = self.entry(package);
        owned(package, &entry)?;
        match std::fs::remove_file(&entry) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("cannot remove {}: {error}", entry.display()));
            }
            _ => {}
        }
        self.remove_icons(package, None)
    }

    /// Removes the icons of `package` in every format but `keep`.
    fn remove_icons(&self, package: &str, keep: Option<ImageFormat>) -> Result<(), String> {
        for format in FORMATS.into_iter().filter(|format| Some(*format) != keep) {
            let path = self.icon(package, format);
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("cannot remove {}: {error}", path.display()));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Whether `entry` is free for uxdroid to write: absent, or a file uxdroid wrote for `package`. A
/// symlink or anything else is the user's own, and the error says it is left alone.
fn owned(package: &str, entry: &Path) -> Result<(), String> {
    let left = || {
        Err(format!(
            "{} was not written by uxdroid; it is left as it is",
            entry.display()
        ))
    };
    match std::fs::symlink_metadata(entry) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot read {}: {error}", entry.display())),
        Ok(meta) if !meta.file_type().is_file() => left(),
        Ok(_) => {
            let text = std::fs::read_to_string(entry)
                .map_err(|error| format!("cannot read {}: {error}", entry.display()))?;
            let marker = format!("X-Uxdroid-Package={package}");
            if text.lines().any(|line| line == marker) {
                Ok(())
            } else {
                left()
            }
        }
    }
}

/// A label as one line: control characters become spaces, the ends are trimmed and the length cut.
fn clean(label: &str) -> String {
    let line: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NAME_CHARS)
        .collect();
    line.trim().to_string()
}

/// The escapes of a desktop entry's string values: a backslash doubled. Values never hold control
/// characters here, so no other escape is needed.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\")
}

/// One argument of `Exec`, quoted when it holds a character the specification reserves, and its
/// `%` doubled so it is never a field code.
fn exec_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-+,:=@".contains(c));
    if plain {
        return word.to_string();
    }
    let mut quoted = String::from("\"");
    for c in word.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_words_with_reserved_characters_are_quoted_and_escaped() {
        assert_eq!(exec_word("/usr/bin/uxdroid"), "/usr/bin/uxdroid");
        assert_eq!(
            exec_word("/opt/my apps/ux$droid%\"`\\"),
            "\"/opt/my apps/ux\\$droid%%\\\"\\`\\\\\""
        );
        // The string escape then doubles each backslash: four for one literal backslash.
        assert_eq!(escape(&exec_word("/a\\b")), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn labels_become_one_trimmed_line_of_bounded_length() {
        assert_eq!(clean(" Evil\nExec=x\t"), "Evil Exec=x");
        assert_eq!(clean(&"x".repeat(1000)).len(), MAX_NAME_CHARS);
    }
}
