//! A fake Google Play for tests: a local HTTP server that answers as Play's API, its token
//! dispenser and Google's sign-in do, with the protocol buffer fields uxdroid reads, so no test needs
//! the network or a Google account. It also serves the files of a directory under `/repo/`, so one
//! server can hold an F-Droid repository too.
//!
//! Tests change its answers through [`FakePlay::state`]: the apps it offers, hostile answers for a
//! path, and expired sign-ins.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use sha2::{Digest, Sha256};

/// The device id the fake dispenser and check-in give.
pub const GSF_ID: &str = "3787d285572b6434";
/// What the fake sign-in takes as the account's tokens.
pub const AAS_TOKEN: &str = "aas_et/fake-account-token";
pub const OAUTH_TOKEN: &str = "oauth2_4/fake-oauth-token";

/// A protocol buffer message, written field by field.
#[derive(Default, Clone)]
pub struct Proto(pub Vec<u8>);

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

impl Proto {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn uint(mut self, number: u32, value: u64) -> Self {
        varint(&mut self.0, u64::from(number) << 3);
        varint(&mut self.0, value);
        self
    }

    pub fn fixed64(mut self, number: u32, value: u64) -> Self {
        varint(&mut self.0, (u64::from(number) << 3) | 1);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn bytes(mut self, number: u32, value: &[u8]) -> Self {
        varint(&mut self.0, (u64::from(number) << 3) | 2);
        varint(&mut self.0, value.len() as u64);
        self.0.extend_from_slice(value);
        self
    }

    pub fn string(self, number: u32, value: &str) -> Self {
        self.bytes(number, value.as_bytes())
    }

    pub fn message(self, number: u32, value: Proto) -> Self {
        self.bytes(number, &value.0)
    }
}

/// Base64 with the URL alphabet and no padding, as Play gives hashes.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..=chunk.len() {
            out.push(ALPHABET[(value >> (18 - 6 * index)) as usize & 63] as char);
        }
    }
    out
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

/// An app the fake offers, with the files a delivery gives: a base, then splits.
#[derive(Clone)]
pub struct PlayApp {
    pub package: String,
    pub title: String,
    pub summary: String,
    pub version_code: u64,
    pub version_name: String,
    /// As Play shows it, such as `Mar 10, 2025`.
    pub updated: String,
    /// The SHA-256 of its signing certificate, lowercase hex, as the details give it.
    pub signer: String,
    /// The price in millionths, 0 when free, and as shown.
    pub price_micros: u64,
    pub price_shown: String,
    /// `base` first, then each split by name.
    pub files: Vec<(String, Vec<u8>)>,
    /// The delivery's status: 1 delivers, anything else refuses.
    pub delivery_status: u64,
    /// What the delivery states of each file instead of the truth, by its name.
    pub stated_sha256: BTreeMap<String, Vec<u8>>,
    pub stated_size: BTreeMap<String, u64>,
    /// The address the delivery gives each file, instead of the fake's own, by its name.
    pub urls: BTreeMap<String, String>,
}

impl PlayApp {
    /// A free app with one APK, `base`, signed by the certificate with SHA-256 `signer`.
    pub fn new(
        package: &str,
        title: &str,
        code: u64,
        name: &str,
        apk: Vec<u8>,
        signer: &str,
    ) -> Self {
        Self {
            package: package.to_string(),
            title: title.to_string(),
            summary: format!("{title}, from the fake Google Play"),
            version_code: code,
            version_name: name.to_string(),
            updated: "Mar 10, 2025".to_string(),
            signer: signer.to_string(),
            price_micros: 0,
            price_shown: String::new(),
            files: vec![("base".to_string(), apk)],
            delivery_status: 1,
            stated_sha256: BTreeMap::new(),
            stated_size: BTreeMap::new(),
            urls: BTreeMap::new(),
        }
    }

    pub fn split(mut self, name: &str, apk: Vec<u8>) -> Self {
        self.files.push((name.to_string(), apk));
        self
    }

    /// The app as a details answer and search results describe it.
    fn item(&self) -> Proto {
        let mut details = Proto::new()
            .string(1, "Fake developer")
            .uint(3, self.version_code)
            .string(4, &self.version_name)
            .uint(9, self.files.iter().map(|(_, apk)| apk.len() as u64).sum())
            .string(14, &self.package)
            .string(16, &self.updated);
        for (name, apk) in &self.files {
            let mut file = Proto::new()
                .uint(1, 0)
                .uint(2, self.version_code)
                .uint(3, apk.len() as u64);
            if name != "base" {
                file = file.string(4, name);
            }
            details = details.message(17, file);
        }
        details = details.message(
            22,
            Proto::new()
                .string(1, "c2hhMS1ub3QtcmVhZA")
                .string(2, &base64url(&hex_bytes(&self.signer))),
        );
        let offer = Proto::new()
            .uint(1, self.price_micros)
            .string(2, "USD")
            .string(3, &self.price_shown)
            .uint(8, 1);
        Proto::new()
            .string(1, &self.package)
            .string(2, &self.package)
            .uint(3, 1)
            .string(5, &self.title)
            .string(6, "Fake developer")
            .message(8, offer)
            .message(13, Proto::new().message(1, details))
            .string(27, &self.summary)
    }
}

/// One request the fake received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// The path with its query.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A hostile or failing answer for every request whose path starts with a prefix.
#[derive(Debug, Clone)]
pub enum Answer {
    /// These bytes, with status 200.
    Raw(Vec<u8>),
    /// This status and no body.
    Status(u16),
    /// A status 200 promising this many bytes and sending them, as zeros.
    Zeros(u64),
}

/// What the fake offers and has seen. Tests change it between commands.
#[derive(Default)]
pub struct State {
    pub apps: Vec<PlayApp>,
    pub requests: Vec<Recorded>,
    pub answers: Vec<(String, Answer)>,
    /// Sign-ins handed out; the last is the only one the API takes.
    pub dispensed: u32,
    /// Packages acquired, which a delivery needs.
    pub acquired: Vec<String>,
    /// The bodies the dispenser was sent.
    pub profiles: Vec<serde_json::Value>,
}

impl State {
    fn token(&self) -> String {
        format!("fake-auth-{}", self.dispensed)
    }
}

pub struct FakePlay {
    port: u16,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakePlay {
    /// Starts the fake, serving `repo`, when it is given, under `/repo/`.
    pub fn start(repo: Option<&Path>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop, repo) = (state.clone(), stop.clone(), repo.map(Path::to_path_buf));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let (state, repo) = (state.clone(), repo.clone());
                        std::thread::spawn(move || serve(stream, &state, port, repo.as_deref()));
                    }
                }
            })
        };
        Self {
            port,
            state,
            stop,
            thread: Some(thread),
        }
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The API's address, to put in the Play source.
    pub fn api(&self) -> String {
        format!("{}/fdfe", self.origin())
    }

    pub fn dispenser(&self) -> String {
        format!("{}/api/auth", self.origin())
    }

    pub fn repo(&self) -> String {
        format!("{}/repo", self.origin())
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn add(&self, app: PlayApp) {
        let mut state = self.state();
        state.apps.retain(|seen| seen.package != app.package);
        state.apps.push(app);
    }

    pub fn answer(&self, prefix: &str, answer: Answer) {
        self.state().answers.push((prefix.to_string(), answer));
    }

    /// The requests since the last call.
    pub fn take(&self) -> Vec<Recorded> {
        std::mem::take(&mut self.state().requests)
    }

    /// Makes every sign-in handed out so far expire.
    pub fn expire(&self) {
        self.state().dispensed += 1;
    }
}

impl Drop for FakePlay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &TcpStream) -> Option<Recorded> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.trim_end().split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length.min(1 << 20)];
    reader.read_exact(&mut body).ok()?;
    Some(Recorded {
        method,
        path,
        headers,
        body,
    })
}

fn respond(mut stream: &TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
}

fn query(path: &str, name: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| decode(value))
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 3 <= bytes.len() => {
                out.push(
                    std::str::from_utf8(&bytes[at + 1..at + 3])
                        .ok()
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                        .unwrap_or(b'?'),
                );
                at += 3;
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn wrapper(payload: Proto) -> Vec<u8> {
    Proto::new().message(1, payload).0
}

fn serve(stream: TcpStream, state: &Mutex<State>, port: u16, repo: Option<&Path>) {
    let Some(request) = read_request(&stream) else {
        return;
    };
    let mut state = state.lock().unwrap();
    state.requests.push(request.clone());
    let path = request.path.clone();
    if let Some((_, answer)) = state
        .answers
        .iter()
        .find(|(prefix, _)| path.starts_with(prefix.as_str()))
        .cloned()
    {
        drop(state);
        match answer {
            Answer::Raw(bytes) => respond(&stream, 200, "application/protobuf", &bytes),
            Answer::Status(code) => respond(&stream, code, "text/plain", b""),
            Answer::Zeros(count) => {
                let mut out = &stream;
                let _ = write!(
                    out,
                    "HTTP/1.1 200 OK\r\nContent-Length: {count}\r\nConnection: close\r\n\r\n"
                );
                let chunk = vec![0u8; 64 * 1024];
                let mut left = count;
                while left > 0 {
                    let part = left.min(chunk.len() as u64) as usize;
                    if out.write_all(&chunk[..part]).is_err() {
                        break;
                    }
                    left -= part as u64;
                }
            }
        }
        return;
    }
    let route = path.split('?').next().unwrap_or_default().to_string();
    if let Some(name) = route.strip_prefix("/repo/") {
        let body = repo
            .filter(|_| !name.contains(".."))
            .and_then(|repo| std::fs::read(repo.join(name)).ok());
        match body {
            Some(body) => respond(&stream, 200, "application/octet-stream", &body),
            None => respond(&stream, 404, "text/plain", b""),
        }
        return;
    }
    if route == "/api/auth" && request.method == "POST" {
        if let Ok(profile) = serde_json::from_slice(&request.body) {
            state.profiles.push(profile);
        }
        state.dispensed += 1;
        let answer = serde_json::json!({
            "email": "shared@example.org",
            "authToken": state.token(),
            "gsfId": GSF_ID,
            "deviceCheckInConsistencyToken": "fake-checkin",
            "deviceConfigToken": "fake-config",
            "dfeCookie": "fake-cookie",
            "isAnonymous": false,
        });
        respond(
            &stream,
            200,
            "application/json",
            answer.to_string().as_bytes(),
        );
        return;
    }
    if route == "/checkin" {
        let answer = Proto::new()
            .uint(1, 1)
            .fixed64(7, u64::from_str_radix(GSF_ID, 16).unwrap())
            .fixed64(8, 99)
            .string(12, "fake-checkin");
        respond(&stream, 200, "application/x-protobuffer", &answer.0);
        return;
    }
    if route == "/auth" {
        let form = String::from_utf8_lossy(&request.body).into_owned();
        let field = |name: &str| query(&format!("?{form}"), name);
        let answer = match field("Token").as_deref() {
            Some(OAUTH_TOKEN) if field("ACCESS_TOKEN").as_deref() == Some("1") => {
                format!("Token={AAS_TOKEN}\nEmail=me@example.org\n")
            }
            Some(AAS_TOKEN)
                if field("service").is_some_and(|scope| scope.contains("googleplay")) =>
            {
                state.dispensed += 1;
                format!("Auth={}\nExpiry=0\n", state.token())
            }
            _ => {
                respond(&stream, 403, "text/plain", b"Error=BadAuthentication\n");
                return;
            }
        };
        respond(&stream, 200, "text/plain", answer.as_bytes());
        return;
    }
    if let Some(name) = route.strip_prefix("/download/") {
        let (package, file) = name.split_once('/').unwrap_or_default();
        let body = state
            .apps
            .iter()
            .find(|app| app.package == package)
            .and_then(|app| app.files.iter().find(|(seen, _)| seen == file))
            .map(|(_, apk)| apk.clone());
        match body {
            Some(body) if request.header("Cookie") == Some("a=42") => respond(
                &stream,
                200,
                "application/vnd.android.package-archive",
                &body,
            ),
            Some(_) => respond(&stream, 403, "text/plain", b""),
            None => respond(&stream, 404, "text/plain", b""),
        }
        return;
    }
    let Some(api) = route.strip_prefix("/fdfe/") else {
        respond(&stream, 404, "text/plain", b"");
        return;
    };
    let token = format!("Bearer {}", state.token());
    if request.header("Authorization") != Some(token.as_str())
        || request.header("X-DFE-Device-Id") != Some(GSF_ID)
    {
        respond(&stream, 401, "text/plain", b"");
        return;
    }
    let find = |package: &Option<String>| {
        state
            .apps
            .iter()
            .find(|app| Some(&app.package) == package.as_ref())
            .cloned()
    };
    match api {
        "details" => match find(&query(&path, "doc")) {
            Some(app) => {
                let answer = wrapper(Proto::new().message(2, Proto::new().message(4, app.item())));
                respond(&stream, 200, "application/protobuf", &answer);
            }
            None => respond(&stream, 404, "text/plain", b""),
        },
        "search" => {
            let text = query(&path, "q").unwrap_or_default().to_lowercase();
            let mut cluster = Proto::new().string(5, "Results");
            for app in &state.apps {
                if app.title.to_lowercase().contains(&text) || app.package.contains(&text) {
                    cluster = cluster.message(11, app.item());
                }
            }
            let list = Proto::new().message(2, Proto::new().message(11, cluster));
            let inner = wrapper(Proto::new().message(1, list));
            let answer = Proto::new()
                .message(1, Proto::new().message(5, Proto::new().string(1, &text)))
                .message(
                    3,
                    Proto::new()
                        .string(1, &format!("searchList?q={text}"))
                        .bytes(2, &inner),
                );
            respond(&stream, 200, "application/protobuf", &answer.0);
        }
        "purchase" => {
            let form = String::from_utf8_lossy(&request.body).into_owned();
            let package = query(&format!("?{form}"), "doc");
            match find(&package) {
                Some(app) if app.price_micros == 0 => {
                    state.acquired.push(app.package.clone());
                    let answer = wrapper(Proto::new().message(4, Proto::new().uint(1, 1)));
                    respond(&stream, 200, "application/protobuf", &answer);
                }
                Some(_) => respond(&stream, 403, "text/plain", b""),
                None => respond(&stream, 404, "text/plain", b""),
            }
        }
        "delivery" => {
            let package = query(&path, "doc");
            let code = query(&path, "vc").and_then(|code| code.parse::<u64>().ok());
            let Some(app) = find(&package) else {
                respond(&stream, 404, "text/plain", b"");
                return;
            };
            if !state.acquired.contains(&app.package) || code != Some(app.version_code) {
                let answer = wrapper(Proto::new().message(21, Proto::new().uint(1, 3)));
                respond(&stream, 200, "application/protobuf", &answer);
                return;
            }
            let sha256 = |name: &str, apk: &[u8]| {
                app.stated_sha256
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| Sha256::digest(apk).to_vec())
            };
            let size = |name: &str, apk: &[u8]| {
                app.stated_size
                    .get(name)
                    .copied()
                    .unwrap_or(apk.len() as u64)
            };
            let url = |name: &str| {
                app.urls.get(name).cloned().unwrap_or_else(|| {
                    format!("http://127.0.0.1:{port}/download/{}/{name}", app.package)
                })
            };
            let (_, base) = &app.files[0];
            let mut data = Proto::new()
                .uint(1, size("base", base))
                .string(3, &url("base"))
                .message(5, Proto::new().string(1, "a").string(2, "42"))
                .string(19, &base64url(&sha256("base", base)))
                .uint(29, app.version_code);
            for (name, apk) in &app.files[1..] {
                data = data.message(
                    15,
                    Proto::new()
                        .string(1, name)
                        .uint(2, size(name, apk))
                        .string(5, &url(name))
                        .string(9, &base64url(&sha256(name, apk))),
                );
            }
            let answer = wrapper(Proto::new().message(
                21,
                Proto::new().uint(1, app.delivery_status).message(2, data),
            ));
            respond(&stream, 200, "application/protobuf", &answer);
        }
        "uploadDeviceConfig" => {
            let answer = wrapper(Proto::new().message(28, Proto::new().string(1, "fake-config")));
            respond(&stream, 200, "application/protobuf", &answer);
        }
        _ => respond(&stream, 404, "text/plain", b""),
    }
}
