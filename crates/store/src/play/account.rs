//! How the Play source signs in, and what it keeps of it in `<data>/store/play`:
//!
//! - `settings.json`: anonymous or an account, and the token dispenser anonymous mode asks.
//! - `token`: the account's AAS token, given by the user; never a password.
//! - `session.json`: the sign-in in use, for the device profile it was made for.
//!
//! The directory is readable only by the user, and so are the token and the session, which are
//! never printed. Anonymous mode asks a token dispenser, by default Aurora OSS's, which signs in to
//! one of its shared accounts for the profile it is sent and hands back that sign-in. An account
//! signs in itself: Android's check-in gives the device an id, `uploadDeviceConfig` registers the
//! profile, and the AAS token buys an access token for Play.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::proto::{Message, Writer};
use super::{Profile, malformed_json};
use crate::config::normalize_url;
use crate::fetch::{Fetch, Request, Status};
use crate::{Error, write_atomically};

/// Aurora OSS's dispenser, which Aurora Store signs in anonymously with.
pub const DEFAULT_DISPENSER: &str = "https://auroraoss.com/api/auth";
/// The SHA-1 of the certificate Google signs its Android apps with, which Google's sign-in asks
/// a caller to name.
const GOOGLE_SIGNATURE: &str = "38918a453d07199354f8b19af05ec6562ced5788";
const PLAY_SCOPE: &str = "oauth2:https://www.googleapis.com/auth/googleplay";
/// Sign-in answers seen are under 16 KiB; this only bounds a hostile server.
const MAX_SIGN_IN_BYTES: u64 = 1024 * 1024;
const MAX_TOKEN: usize = 4096;
const MAX_EMAIL: usize = 254;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Anonymous,
    Account,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub mode: Mode,
    pub dispenser: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            mode: Mode::Anonymous,
            dispenser: DEFAULT_DISPENSER.to_string(),
            email: None,
        }
    }
}

/// A sign-in: what every request to Play carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Session {
    /// The profile it was made for, by [`Profile::digest`].
    pub profile: String,
    pub mode: Mode,
    pub email: String,
    pub auth_token: String,
    /// The device id, in hex.
    pub gsf_id: String,
    #[serde(default)]
    pub checkin_token: String,
    #[serde(default)]
    pub device_config_token: String,
    #[serde(default)]
    pub dfe_cookie: String,
}

pub(crate) fn directory(root: &Path) -> PathBuf {
    root.join("play")
}

/// Creates the directory readable only by the user, and makes it so if it is not.
fn private_directory(root: &Path) -> Result<PathBuf, Error> {
    let directory = directory(root);
    let cannot = |error: std::io::Error| {
        Error::new(format!("cannot write {}: {error}", directory.display()))
    };
    std::fs::create_dir_all(&directory).map_err(cannot)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).map_err(cannot)?;
    Ok(directory)
}

/// Replaces `path` with `bytes`, readable and writable only by the user from its first byte.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let cannot =
        |error: std::io::Error| Error::new(format!("cannot write {}: {error}", path.display()));
    let name = path
        .file_name()
        .map_or_else(Default::default, |name| name.to_string_lossy());
    let temporary = path.with_file_name(format!(".{name}.new-{}", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(cannot)
}

fn read_small(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_SIGN_IN_BYTES => {
            Err(Error::new(format!("{} is too large", path.display())))
        }
        Ok(_) => std::fs::read(path)
            .map(Some)
            .map_err(|error| Error::new(format!("cannot read {}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::new(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

impl Settings {
    pub fn path(root: &Path) -> PathBuf {
        directory(root).join("settings.json")
    }

    /// The settings of the store at `root`, or the default ones, anonymous through
    /// [`DEFAULT_DISPENSER`].
    pub fn load(root: &Path) -> Result<Self, Error> {
        let path = Self::path(root);
        let Some(bytes) = read_small(&path)? else {
            return Ok(Self::default());
        };
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(format!("{} is malformed: {error}", path.display())))?;
        let valid = settings.version == 1
            && normalize_url(&settings.dispenser).as_deref() == Ok(settings.dispenser.as_str())
            && match settings.mode {
                Mode::Anonymous => settings.email.is_none(),
                Mode::Account => settings.email.as_deref().is_some_and(valid_email),
            };
        if !valid {
            return Err(Error::new(format!("{} is malformed", path.display())));
        }
        Ok(settings)
    }

    fn save(&self, root: &Path) -> Result<(), Error> {
        private_directory(root)?;
        let mut bytes = serde_json::to_vec_pretty(self).expect("settings serialize");
        bytes.push(b'\n');
        write_atomically(&Self::path(root), &bytes)
    }
}

pub(crate) fn valid_email(email: &str) -> bool {
    email.len() <= MAX_EMAIL
        && email.bytes().all(|byte| byte.is_ascii_graphic())
        && email.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.contains('@')
        })
}

fn token_path(root: &Path) -> PathBuf {
    directory(root).join("token")
}

pub(crate) fn session_path(root: &Path) -> PathBuf {
    directory(root).join("session.json")
}

fn forget(root: &Path) -> Result<(), Error> {
    for path in [token_path(root), session_path(root)] {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::new(format!(
                    "cannot remove {}: {error}",
                    path.display()
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Signs in anonymously from now on, through `dispenser` or the one already set, and forgets any
/// account token and session.
pub fn use_anonymous(root: &Path, dispenser: Option<&str>) -> Result<Settings, Error> {
    let mut settings = Settings::load(root).unwrap_or_default();
    if let Some(dispenser) = dispenser {
        settings.dispenser = normalize_url(dispenser)?;
    }
    settings.mode = Mode::Anonymous;
    settings.email = None;
    forget(root)?;
    settings.save(root)?;
    Ok(settings)
}

/// Changes the dispenser anonymous mode asks, keeping the mode.
pub fn set_dispenser(root: &Path, dispenser: &str) -> Result<Settings, Error> {
    let mut settings = Settings::load(root)?;
    settings.dispenser = normalize_url(dispenser)?;
    if settings.mode == Mode::Anonymous {
        forget(root)?;
    }
    settings.save(root)?;
    Ok(settings)
}

/// Checks a token a user gives: printable, of a known kind, not too long.
fn token_kind(token: &str) -> Result<TokenKind, Error> {
    if token.len() > MAX_TOKEN || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(Error::new(
            "the token must be one line of printable characters, at most 4096",
        ));
    }
    if token.starts_with("aas_et/") {
        Ok(TokenKind::Aas)
    } else if token.starts_with("oauth2_4/") {
        Ok(TokenKind::OAuth)
    } else {
        Err(Error::new(
            "the token is neither an AAS token (aas_et/...) nor the oauth_token cookie of Google's sign-in page (oauth2_4/...); README.md says how to get one",
        ))
    }
}

enum TokenKind {
    Aas,
    OAuth,
}

/// The address of Google's sign-in services next to the Play API at `api`, such as
/// `https://android.clients.google.com/auth`.
fn beside(api: &str, name: &str) -> String {
    let base = api.strip_suffix("/fdfe").unwrap_or(api);
    format!("{base}/{name}")
}

fn form(fields: &[(&str, &str)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

/// Percent-encodes everything but unreserved characters.
pub(crate) fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Posts to Google's sign-in service and reads its `key=value` lines.
fn sign_in_service(
    fetch: &dyn Fetch,
    url: &str,
    fields: &[(&str, &str)],
    profile: &Profile,
) -> Result<Vec<(String, String)>, Error> {
    let request = Request::post(url, form(fields))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", profile.auth_user_agent())
        .header("app", "com.google.android.gms");
    let mut body = Vec::new();
    match fetch.send(&request, MAX_SIGN_IN_BYTES, &mut body)? {
        Status::Ok(_) => {}
        Status::Other(code) => {
            return Err(Error::new(format!(
                "Google's sign-in refused the account's token (it answered {code}): it may have expired or been revoked; uxdroid sources play --account <email> takes a new one"
            )));
        }
    }
    let text = String::from_utf8_lossy(&body);
    Ok(text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect())
}

fn field<'a>(lines: &'a [(String, String)], key: &str) -> Option<&'a str> {
    lines
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

/// Signs in as `email` from now on, with `token`: an AAS token, kept, or an OAuth token from
/// Google's sign-in page, traded for an AAS token first. Only the AAS token is kept.
pub fn use_account(
    fetch: &dyn Fetch,
    api: &str,
    root: &Path,
    profile: &Profile,
    email: &str,
    token: &str,
) -> Result<Settings, Error> {
    if !valid_email(email) {
        return Err(Error::new(format!(
            "{} is not an email address",
            email.escape_debug()
        )));
    }
    let token = token.trim();
    let aas = match token_kind(token)? {
        TokenKind::Aas => token.to_string(),
        TokenKind::OAuth => {
            let lines = sign_in_service(
                fetch,
                &beside(api, "auth"),
                &[
                    ("Email", email),
                    ("Token", token),
                    ("ACCESS_TOKEN", "1"),
                    ("add_account", "1"),
                    ("get_accountid", "1"),
                    ("service", "ac2dm"),
                    ("app", "com.google.android.gms"),
                    ("client_sig", GOOGLE_SIGNATURE),
                    ("callerPkg", "com.google.android.gms"),
                    ("callerSig", GOOGLE_SIGNATURE),
                    ("google_play_services_version", profile.gsf_version()),
                ],
                profile,
            )?;
            let aas = field(&lines, "Token")
                .ok_or_else(|| Error::new("Google's sign-in gave no AAS token for that token"))?;
            token_kind(aas)
                .ok()
                .filter(|kind| matches!(kind, TokenKind::Aas))
                .ok_or_else(|| Error::new("Google's sign-in gave a malformed AAS token"))?;
            aas.to_string()
        }
    };
    let mut settings = Settings::load(root).unwrap_or_default();
    settings.mode = Mode::Account;
    settings.email = Some(email.to_string());
    forget(root)?;
    private_directory(root)?;
    write_private(&token_path(root), aas.as_bytes())?;
    settings.save(root)?;
    Ok(settings)
}

/// The kept session, when it was made for `profile` and the current settings.
pub(crate) fn kept_session(root: &Path, profile: &Profile, settings: &Settings) -> Option<Session> {
    let bytes = read_small(&session_path(root)).ok()??;
    let session: Session = serde_json::from_slice(&bytes).ok()?;
    let email_matches = match settings.mode {
        Mode::Anonymous => true,
        Mode::Account => settings.email.as_deref() == Some(session.email.as_str()),
    };
    (session.profile == profile.digest() && session.mode == settings.mode && email_matches)
        .then_some(session)
}

pub(crate) fn keep_session(root: &Path, session: &Session) -> Result<(), Error> {
    private_directory(root)?;
    let bytes = serde_json::to_vec(session).expect("a session serializes");
    write_private(&session_path(root), &bytes)
}

/// What a dispenser answers; only these fields are read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dispensed {
    #[serde(default)]
    email: String,
    #[serde(default)]
    auth_token: String,
    #[serde(default)]
    gsf_id: String,
    #[serde(default)]
    device_check_in_consistency_token: String,
    #[serde(default)]
    device_config_token: String,
    #[serde(default)]
    dfe_cookie: String,
}

/// Header values must be printable and of a bounded size, so a dispenser cannot forge headers.
fn header_value(value: &str) -> bool {
    value.len() <= 16 * 1024
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

/// Asks the dispenser for a sign-in for `profile`.
pub(crate) fn dispense(
    fetch: &dyn Fetch,
    settings: &Settings,
    profile: &Profile,
) -> Result<Session, Error> {
    let body = serde_json::to_vec(&profile.properties()).expect("properties serialize");
    let request = Request::post(&settings.dispenser, body)
        .header("Content-Type", "application/json")
        .header("User-Agent", concat!("uxdroid/", env!("CARGO_PKG_VERSION")));
    let mut answer = Vec::new();
    let dispenser = &settings.dispenser;
    match fetch.send(&request, MAX_SIGN_IN_BYTES, &mut answer)? {
        Status::Ok(_) => {}
        Status::Other(code) => {
            return Err(Error::new(format!(
                "the token dispenser {dispenser} answered {code}, so Google Play cannot be signed in to anonymously now; try again later, or use your own account with uxdroid sources play --account <email>"
            )));
        }
    }
    let dispensed: Dispensed =
        serde_json::from_slice(&answer).map_err(|error| malformed_json(dispenser, &error))?;
    let gsf_valid = !dispensed.gsf_id.is_empty()
        && dispensed.gsf_id.len() <= 16
        && dispensed
            .gsf_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    let headers_valid = [
        &dispensed.auth_token,
        &dispensed.device_check_in_consistency_token,
        &dispensed.device_config_token,
        &dispensed.dfe_cookie,
        &dispensed.email,
    ]
    .iter()
    .all(|value| header_value(value));
    if dispensed.auth_token.is_empty() || !gsf_valid || !headers_valid {
        return Err(Error::new(format!(
            "the token dispenser {dispenser} gave a malformed sign-in, so it is not used"
        )));
    }
    Ok(Session {
        profile: profile.digest(),
        mode: Mode::Anonymous,
        email: dispensed.email,
        auth_token: dispensed.auth_token,
        gsf_id: dispensed.gsf_id.to_ascii_lowercase(),
        checkin_token: dispensed.device_check_in_consistency_token,
        device_config_token: dispensed.device_config_token,
        dfe_cookie: dispensed.dfe_cookie,
    })
}

/// Signs in with the account's kept AAS token: check-in, then the access token, then the profile.
pub(crate) fn sign_in(
    fetch: &dyn Fetch,
    api: &str,
    root: &Path,
    settings: &Settings,
    profile: &Profile,
) -> Result<Session, Error> {
    let email = settings.email.clone().unwrap_or_default();
    let aas = read_small(&token_path(root))?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .ok_or_else(|| {
            Error::new(format!(
                "no token is kept for {email}: uxdroid sources play --account {email} takes one"
            ))
        })?;
    // Check-in: a device id for this profile.
    let request = Request::post(beside(api, "checkin"), profile.checkin())
        .header("Content-Type", "application/x-protobuffer")
        .header("User-Agent", "Android-Checkin/2.0");
    let mut answer = Vec::new();
    if let Status::Other(code) = fetch.send(&request, MAX_SIGN_IN_BYTES, &mut answer)? {
        return Err(Error::new(format!(
            "Google's check-in answered {code}, so this device cannot sign in"
        )));
    }
    let checkin = Message::new(&answer);
    let gsf_id = checkin
        .uint(7)?
        .filter(|id| *id != 0)
        .ok_or_else(|| Error::new("Google's check-in gave no device id"))?;
    let checkin_token = checkin.string(12)?.unwrap_or_default().to_string();
    let gsf_id = format!("{gsf_id:x}");
    // The access token Play takes.
    let lines = sign_in_service(
        fetch,
        &beside(api, "auth"),
        &[
            ("Email", &email),
            ("Token", &aas),
            ("service", PLAY_SCOPE),
            ("app", "com.android.vending"),
            ("client_sig", GOOGLE_SIGNATURE),
            ("callerPkg", "com.google.android.gms"),
            ("callerSig", GOOGLE_SIGNATURE),
            ("androidId", &gsf_id),
            ("check_email", "1"),
            ("token_request_options", "CAA4AQ=="),
            ("google_play_services_version", profile.gsf_version()),
        ],
        profile,
    )?;
    let auth_token = field(&lines, "Auth")
        .filter(|token| header_value(token))
        .ok_or_else(|| Error::new("Google's sign-in gave no access token for Google Play"))?
        .to_string();
    let mut session = Session {
        profile: profile.digest(),
        mode: Mode::Account,
        email,
        auth_token,
        gsf_id,
        checkin_token,
        device_config_token: String::new(),
        dfe_cookie: String::new(),
    };
    // The profile, so Play serves the apps and splits for it.
    let configuration = Writer::new()
        .message(1, |configuration| profile.configuration(configuration))
        .finish();
    let answer = super::call(
        fetch,
        api,
        &session,
        profile,
        "uploadDeviceConfig",
        Some(configuration),
    )?
    .map_err(|code| {
        Error::new(format!(
            "Google Play refused this device's profile (it answered {code})"
        ))
    })?;
    session.device_config_token = Message::new(&answer)
        .at(&[1, 28])?
        .map(|response| response.string(1))
        .transpose()?
        .flatten()
        .filter(|token| header_value(token))
        .unwrap_or_default()
        .to_string();
    Ok(session)
}
