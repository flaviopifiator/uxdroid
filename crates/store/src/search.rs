//! Ranks apps against what the user typed. Every word must match the app's name, package or
//! summary; a match in the name counts most, and earlier in the name more.

use crate::{App, Device, Release};

/// One app found, with the release it would install.
#[derive(Debug, Clone)]
pub struct Hit<'a> {
    pub app: &'a App,
    pub release: &'a Release,
    pub score: u32,
}

/// The apps that match, best first, and how many more match but have no release for the device.
#[derive(Debug, Default)]
pub struct Matches<'a> {
    pub hits: Vec<Hit<'a>>,
    pub unfit: usize,
}

fn word_score(word: &str, name: &str, package: &str, summary: &str) -> u32 {
    let last_segment = package.rsplit('.').next().unwrap_or(package);
    if name == word || package == word {
        100
    } else if last_segment == word {
        90
    } else if name.starts_with(word) {
        80
    } else if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.starts_with(word))
    {
        60
    } else if name.contains(word) {
        45
    } else if package.contains(word) {
        40
    } else if summary.contains(word) {
        15
    } else {
        0
    }
}

/// The score of `app` for `words`, already lowercase, or none when a word does not match.
fn score(app: &App, words: &[String]) -> Option<u32> {
    let name = app.name.to_lowercase();
    let package = app.package.to_ascii_lowercase();
    let summary = app.summary.to_lowercase();
    let mut total = 0;
    for word in words {
        match word_score(word, &name, &package, &summary) {
            0 => return None,
            score => total += score,
        }
    }
    Some(total)
}

/// The apps of `apps` matching `text`, best first: by score, then name and package, then the order
/// of their sources, as `apps` lists them.
pub fn search<'a>(apps: &'a [App], text: &str, device: &Device) -> Matches<'a> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    let mut matches = Matches::default();
    if words.is_empty() {
        return matches;
    }
    for app in apps {
        let Some(score) = score(app, &words) else {
            continue;
        };
        match app.newest(device) {
            Some(release) => matches.hits.push(Hit {
                app,
                release,
                score,
            }),
            None => matches.unfit += 1,
        }
    }
    // A stable sort keeps the sources' order among equal apps.
    matches.hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.app.name.to_lowercase().cmp(&b.app.name.to_lowercase()))
            .then_with(|| a.app.package.cmp(&b.app.package))
    });
    matches
}
