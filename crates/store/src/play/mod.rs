//! Google Play, through a client of uxdroid's own.
//!
//! Play has no index to download: it is asked app by app, over the API its store app uses
//! (`https://android.clients.google.com/fdfe`), with protocol buffer answers. This client was
//! written from public descriptions of that protocol and from observing its answers, and reads only
//! the fields listed here, each found by its contents in answers for real apps:
//!
//! - Every answer is a wrapper whose field 1 is the payload: 2 a details answer, 4 an acquisition,
//!   21 a delivery; search results come in field 3, pre-fetched answers, each a wrapper in its 2.
//! - An app (`details` 1.2.4, and in search lists inside clusters, field 11) has 1 its package, 5
//!   its title, 27 its short description, 8 its offers (1 the price in millionths, 2 the currency,
//!   3 the price as shown) and 13.1 its details: 3 version code, 4 version name, 9 size, 14 package,
//!   16 the date of its last update as shown, 17 its files (3 the size of each) and 22 its
//!   signing certificate (2 its SHA-256, base64url).
//! - A delivery (1.21) has 1 a status, 1 meaning delivered, and 2 the files: 1 size, 2 SHA-1, 3
//!   address, 5 a cookie (1 name, 2 value), 19 SHA-256, and in 15 each split: 1 name, 2 size, 4
//!   SHA-1, 5 address, 9 SHA-256. Hashes are base64url.
//!
//! Every file Play delivers is downloaded up to the size it states, must have the hashes it states,
//! and then goes through the installer like any APK: signatures, one signer for base and splits,
//! the installed app's signer on an update, and the certificate the details name: the APK's
//! signer, or for an app that rotated its key, as Signal did, the certificate its proof of
//! rotation starts from, which is what Play names (Signal's details name `29f34e5f…`, its APK's
//! signer for SDK 36 is `4be4f6cd…`).

pub mod account;
mod profile;
pub(crate) mod proto;

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use account::{Settings, use_account};
pub use profile::Profile;
use proto::{Message, base64url};

use crate::config::SourceConfig;
use crate::fdroid::MAX_APK_BYTES;
use crate::fetch::{Fetch, Request, Status};
use crate::{App, Error, Release, valid_package};

/// Answers seen are under 100 KiB (a search, 66 KiB); this only bounds a hostile server.
const MAX_ANSWER_BYTES: u64 = 8 * 1024 * 1024;
/// Play is asked at most this often, so a long update does not hammer it.
const PACE: Duration = Duration::from_millis(250);
/// More splits than any app has; a delivery with more is refused.
const MAX_FILES: usize = 64;
const MAX_RESULTS: usize = 100;
const MAX_SPLIT_NAME: usize = 255;

static LAST_CALL: Mutex<Option<Instant>> = Mutex::new(None);

fn pace() {
    let mut last = LAST_CALL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(wait) = last.and_then(|last| PACE.checked_sub(last.elapsed())) {
        std::thread::sleep(wait);
    }
    *last = Some(Instant::now());
}

/// Asks Play's API at `api` for `path`, posting `body` when there is one: a form, or a protocol
/// buffer when it starts as one. Another status than success is returned as such.
pub(crate) fn call(
    fetch: &dyn Fetch,
    api: &str,
    session: &account::Session,
    profile: &Profile,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<Result<Vec<u8>, u16>, Error> {
    let url = format!("{api}/{path}");
    let content_type = if path == "uploadDeviceConfig" {
        "application/x-protobuf"
    } else {
        "application/x-www-form-urlencoded; charset=UTF-8"
    };
    let mut request = match body {
        Some(body) => Request::post(&url, body).header("Content-Type", content_type),
        None => Request::get(&url),
    }
    .header("Authorization", format!("Bearer {}", session.auth_token))
    .header("User-Agent", profile.user_agent())
    .header("X-DFE-Device-Id", &session.gsf_id)
    .header("Accept-Language", "en-US")
    .header("X-DFE-Client-Id", "am-android-google")
    .header("X-DFE-Network-Type", "4")
    .header("X-DFE-Content-Filters", "")
    .header("X-DFE-UserLanguages", profile.user_languages())
    .header("X-DFE-Request-Params", "timeoutMs=4000");
    for (name, value) in [
        (
            "X-DFE-Device-Checkin-Consistency-Token",
            &session.checkin_token,
        ),
        ("X-DFE-Device-Config-Token", &session.device_config_token),
        ("X-DFE-Cookie", &session.dfe_cookie),
    ] {
        if !value.is_empty() {
            request = request.header(name, value);
        }
    }
    pace();
    let mut answer = Vec::new();
    Ok(match fetch.send(&request, MAX_ANSWER_BYTES, &mut answer)? {
        Status::Ok(_) => {
            // A cut or damaged answer is refused whole, before any field of it is used.
            Message::new(&answer).check()?;
            Ok(answer)
        }
        Status::Other(code) => Err(code),
    })
}

/// One file of a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct File {
    /// `base`, or the split's name.
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<Vec<u8>>,
    pub sha1: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub files: Vec<File>,
    /// A cookie the downloads carry.
    pub cookie: Option<String>,
}

/// The Play source of a store, for one command.
pub(crate) struct Play<'a> {
    pub source: &'a SourceConfig,
    pub root: &'a Path,
    pub profile: &'a Profile,
    pub fetch: &'a dyn Fetch,
    pub session: std::cell::RefCell<Option<account::Session>>,
}

impl<'a> Play<'a> {
    pub(crate) fn new(
        source: &'a SourceConfig,
        root: &'a Path,
        profile: &'a Profile,
        fetch: &'a dyn Fetch,
    ) -> Self {
        Self {
            source,
            root,
            profile,
            fetch,
            session: std::cell::RefCell::new(None),
        }
    }

    /// The kept sign-in, or a new one when there is none or `fresh` asks for it.
    fn session(&self, fresh: bool) -> Result<account::Session, Error> {
        if !fresh && let Some(session) = self.session.borrow().clone() {
            return Ok(session);
        }
        let settings = Settings::load(self.root)?;
        let kept = (!fresh)
            .then(|| account::kept_session(self.root, self.profile, &settings))
            .flatten();
        let session = match kept {
            Some(session) => session,
            None => {
                let session = account::sign_in(
                    self.fetch,
                    &self.source.url,
                    self.root,
                    &settings,
                    self.profile,
                )?;
                account::keep_session(self.root, &session)?;
                session
            }
        };
        *self.session.borrow_mut() = Some(session.clone());
        Ok(session)
    }

    /// Asks the API, signing in again once when the sign-in has expired. Another status than
    /// success is returned as such.
    fn api(&self, path: &str, body: Option<Vec<u8>>) -> Result<Result<Vec<u8>, u16>, Error> {
        let session = self.session(false)?;
        let api = &self.source.url;
        match call(self.fetch, api, &session, self.profile, path, body.clone())? {
            Err(401 | 403) => {
                let session = self.session(true)?;
                call(self.fetch, api, &session, self.profile, path, body)
            }
            answer => Ok(answer),
        }
    }

    fn refused(&self, what: &str, code: u16) -> Error {
        let hint = if matches!(code, 401 | 403) {
            ": the sign-in was refused; Google may have limited the account"
        } else if code == 429 {
            ": it is asked too often; try again later"
        } else {
            ""
        };
        Error::new(format!("Google Play answered {code} to {what}{hint}"))
    }

    /// The apps Play finds for `text`, in its order.
    pub(crate) fn search(&self, text: &str) -> Result<Vec<App>, Error> {
        let path = format!("search?c=3&q={}&ksm=1", account::encode(text));
        let answer = self
            .api(&path, None)?
            .map_err(|code| self.refused("a search", code))?;
        let wrapper = Message::new(&answer);
        let mut lists = Vec::new();
        lists.extend(wrapper.at(&[1, 1])?);
        for prefetch in wrapper.messages(3)? {
            if let Some(inner) = prefetch.message(2)? {
                lists.extend(inner.at(&[1, 1])?);
            }
        }
        let mut items = Vec::new();
        for list in lists {
            for document in list.messages(2)? {
                collect_items(document, 0, &mut items)?;
            }
        }
        let mut apps: Vec<App> = Vec::new();
        for item in items {
            if let Some(app) = app_of(item, &self.source.name)?
                && !apps.iter().any(|seen| seen.package == app.package)
            {
                apps.push(app);
                if apps.len() == MAX_RESULTS {
                    break;
                }
            }
        }
        Ok(apps)
    }

    /// `package` as Play offers it to this device, or none when Play does not have it.
    pub(crate) fn details(&self, package: &str) -> Result<Option<App>, Error> {
        if !valid_package(package) {
            return Ok(None);
        }
        let answer = match self.api(&format!("details?doc={package}"), None)? {
            Ok(answer) => answer,
            Err(404) => return Ok(None),
            Err(code) => return Err(self.refused(&format!("the details of {package}"), code)),
        };
        let Some(item) = Message::new(&answer).at(&[1, 2, 4])? else {
            return Ok(None);
        };
        match app_of(item, &self.source.name)? {
            Some(app) if app.package == package => Ok(Some(app)),
            Some(app) => Err(Error::new(format!(
                "Google Play answered for {} when asked for {package}, so it is refused",
                app.package
            ))),
            None => Err(Error::new(format!(
                "Google Play has {package} but offers no version of it to this device ({}, SDK {})",
                self.profile.abi, self.profile.sdk
            ))),
        }
    }

    /// Acquires `release` of `app`, free, and asks where its files are.
    pub(crate) fn deliver(&self, app: &App, release: &Release) -> Result<Delivery, Error> {
        let package = &app.package;
        if let Some(price) = &release.paid {
            return Err(Error::new(format!(
                "{package} costs {price} on Google Play, and uxdroid installs only free apps from it"
            )));
        }
        let code = release.version_code;
        let form = format!("ot=1&doc={package}&vc={code}").into_bytes();
        self.api("purchase", Some(form))?
            .map_err(|status| self.refused(&format!("acquiring {package}"), status))?;
        let answer = self
            .api(&format!("delivery?ot=1&doc={package}&vc={code}"), None)?
            .map_err(|status| self.refused(&format!("delivering {package}"), status))?;
        let delivery = Message::new(&answer).at(&[1, 21])?.unwrap_or_default();
        let status = delivery.uint(1)?.unwrap_or(0);
        let data = match delivery.message(2)? {
            Some(data) if status == 1 => data,
            _ => {
                return Err(Error::new(format!(
                    "Google Play does not deliver {package} {} to this device ({}, SDK {}) (status {status}): it may not be available for it or in its country",
                    release.version_code, self.profile.abi, self.profile.sdk
                )));
            }
        };
        self.files(package, data)
    }

    fn files(&self, package: &str, data: Message) -> Result<Delivery, Error> {
        let refuse = |why: String| {
            Error::new(format!(
                "Google Play's delivery of {package} {why}, so it is refused"
            ))
        };
        let hash = |text: Option<&str>, length: usize| -> Result<Option<Vec<u8>>, Error> {
            match text {
                None | Some("") => Ok(None),
                Some(text) => base64url(text)
                    .filter(|bytes| bytes.len() == length)
                    .map(Some)
                    .ok_or_else(|| refuse("has a malformed hash".to_string())),
            }
        };
        let mut files = Vec::new();
        let base = File {
            name: "base".to_string(),
            url: data.string(3)?.unwrap_or_default().to_string(),
            size: data.uint(1)?.unwrap_or(0),
            sha1: hash(data.string(2)?, 20)?,
            sha256: hash(data.string(19)?, 32)?,
        };
        files.push(base);
        for split in data.messages(15)? {
            let name = split.string(1)?.unwrap_or_default();
            if !valid_split_name(name) || files.iter().any(|file: &File| file.name == name) {
                return Err(refuse(format!(
                    "names a split {}, which is not a valid or a new split name",
                    name.escape_debug()
                )));
            }
            files.push(File {
                name: name.to_string(),
                url: split.string(5)?.unwrap_or_default().to_string(),
                size: split.uint(2)?.unwrap_or(0),
                sha1: hash(split.string(4)?, 20)?,
                sha256: hash(split.string(9)?, 32)?,
            });
            if files.len() > MAX_FILES {
                return Err(refuse(format!("has more than {MAX_FILES} files")));
            }
        }
        for file in &files {
            if file.size == 0 || file.size > MAX_APK_BYTES {
                return Err(refuse(format!(
                    "gives {} a size of {} bytes",
                    file.name, file.size
                )));
            }
            if !self.allowed_download(&file.url) {
                return Err(refuse(format!(
                    "gives {} the address {}, which is not Google Play's",
                    file.name,
                    file.url.escape_debug()
                )));
            }
        }
        let cookie = match data.message(5)? {
            Some(cookie) => {
                let name = cookie.string(1)?.unwrap_or_default();
                let value = cookie.string(2)?.unwrap_or_default();
                let token = |text: &str| {
                    text.len() <= 4096
                        && text
                            .bytes()
                            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b';' | b','))
                };
                if name.is_empty() || !token(name) || !token(value) {
                    return Err(refuse("has a malformed cookie".to_string()));
                }
                Some(format!("{name}={value}"))
            }
            None => None,
        };
        Ok(Delivery { files, cookie })
    }

    /// Files come from Play's download host over https, or from the API's own origin, as a test
    /// server serves them.
    fn allowed_download(&self, url: &str) -> bool {
        let origin = |url: &str| -> Option<String> {
            let (scheme, rest) = url.split_once("://")?;
            let host = rest.split(['/', '?', '#']).next()?;
            (!host.is_empty() && !host.contains(['@', '\\'])).then(|| format!("{scheme}://{host}"))
        };
        let printable = url.len() <= 8192 && url.bytes().all(|byte| byte.is_ascii_graphic());
        printable
            && (origin(url).as_deref() == Some("https://play.googleapis.com")
                || origin(url)
                    .is_some_and(|origin| Some(origin) == self::origin_of(&self.source.url)))
    }
}

fn origin_of(api: &str) -> Option<String> {
    let (scheme, rest) = api.split_once("://")?;
    let host = rest.split('/').next()?;
    Some(format!("{scheme}://{host}"))
}

/// Split names become file names in the installer's checks and in error messages.
fn valid_split_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SPLIT_NAME
        && name != "base"
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The apps in a document of a list: the document itself, or the apps of its clusters.
fn collect_items<'a>(
    document: Message<'a>,
    depth: u32,
    items: &mut Vec<Message<'a>>,
) -> Result<(), Error> {
    if items.len() >= MAX_RESULTS * 4 {
        return Ok(());
    }
    if document.at(&[13, 1])?.is_some() {
        items.push(document);
        return Ok(());
    }
    if depth < 4 {
        for child in document.messages(11)? {
            collect_items(child, depth + 1, items)?;
        }
    }
    Ok(())
}

/// An app as Play describes it, when it is an app with a version for this device.
fn app_of(item: Message, source: &str) -> Result<Option<App>, Error> {
    let Some(details) = item.at(&[13, 1])? else {
        return Ok(None);
    };
    let package = match details.string(14)? {
        Some(package) => package,
        None => item.string(1)?.unwrap_or_default(),
    };
    let Some(code) = details.uint(3)?.filter(|code| *code != 0) else {
        return Ok(None);
    };
    if !valid_package(package) {
        return Ok(None);
    }
    let files: u64 = details
        .messages(17)?
        .iter()
        .map(|file| file.uint(3).map(|size| size.unwrap_or(0)))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .fold(0u64, |total, size| total.saturating_add(*size));
    let size = if files > 0 {
        files
    } else {
        details.uint(9)?.unwrap_or(0)
    };
    let paid = match item.messages(8)?.first() {
        Some(offer) if offer.uint(1)?.unwrap_or(0) > 0 => {
            let shown = offer.string(3)?.unwrap_or_default();
            Some(if shown.is_empty() {
                format!(
                    "{:.2} {}",
                    offer.uint(1)?.unwrap_or(0) as f64 / 1e6,
                    offer.string(2)?.unwrap_or_default()
                )
            } else {
                shown.to_string()
            })
        }
        _ => None,
    };
    let mut signers = Vec::new();
    for certificate in details.messages(22)? {
        if let Some(sha256) = certificate
            .string(2)?
            .and_then(base64url)
            .filter(|bytes| bytes.len() == 32)
        {
            signers.push(proto::hex(&sha256));
        }
    }
    let updated = details.string(16)?.and_then(shown_date);
    Ok(Some(App {
        source: source.to_string(),
        package: package.to_string(),
        name: item.string(5)?.unwrap_or(package).to_string(),
        summary: item.string(27)?.unwrap_or_default().to_string(),
        updated,
        license: None,
        releases: vec![Release {
            version_name: details.string(4)?.unwrap_or_default().to_string(),
            version_code: code,
            url: format!("https://play.google.com/store/apps/details?id={package}"),
            sha256: String::new(),
            size,
            min_sdk: 0,
            max_sdk: None,
            abis: Vec::new(),
            signers,
            added: updated,
            stable: true,
            paid,
        }],
    }))
}

/// A date as Play shows it in English, such as `Mar 10, 2025`, in milliseconds since the epoch.
fn shown_date(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (month, rest) = text.split_once(' ')?;
    let (day, year) = rest.split_once(", ")?;
    let month = MONTHS.iter().position(|name| *name == month)? as i64 + 1;
    let day: i64 = day.parse().ok().filter(|day| (1..=31).contains(day))?;
    let year: i64 = year
        .parse()
        .ok()
        .filter(|year| (1970..=9999).contains(year))?;
    // Howard Hinnant's days-from-civil algorithm.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days).ok().map(|days| days * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_as_play_shows_them_are_read() {
        assert_eq!(shown_date("Jan 1, 1970"), Some(0));
        assert_eq!(shown_date("Mar 10, 2025"), Some(1_741_564_800_000));
        assert_eq!(shown_date("Feb 29, 2000"), Some(951_782_400_000));
        assert_eq!(shown_date("10 mar 2025"), None);
        assert_eq!(shown_date("Mar 99, 2025"), None);
    }

    #[test]
    fn split_names_are_checked() {
        assert!(valid_split_name("config.x86_64"));
        assert!(valid_split_name("feature-one"));
        for bad in [
            "",
            "base",
            ".hidden",
            "../x",
            "a/b",
            "a b",
            &"x".repeat(256),
        ] {
            assert!(!valid_split_name(bad), "{bad}");
        }
    }
}
