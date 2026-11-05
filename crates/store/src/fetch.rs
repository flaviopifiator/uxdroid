//! Downloads over HTTP(S), bounded in bytes and time. Nothing fetched is trusted because of how it
//! arrived: callers check signatures and hashes. TLS only keeps what a user looks for private.

use std::io::{Read, Write};
use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

use crate::Error;

/// How a source reads what it names by URL. Tests and other transports implement it too.
pub trait Fetch {
    /// Streams `url` into `sink` and returns the number of bytes written. More than `limit` bytes is
    /// refused, and so is a response that is not a success.
    fn copy(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, Error>;

    /// Reads `url` into memory, refusing more than `limit` bytes.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        self.copy(url, limit, &mut bytes)?;
        Ok(bytes)
    }

    /// Sends `request` and streams a successful response's body into `sink`, refusing more than
    /// `limit` bytes. A response that is not a success is returned by its status, with no body, so
    /// the caller can tell an expired sign-in from a missing app.
    fn send(&self, request: &Request, limit: u64, sink: &mut dyn Write) -> Result<Status, Error> {
        let _ = (limit, sink);
        Err(Error::new(format!(
            "cannot fetch {}: this transport sends no requests",
            request.url
        )))
    }
}

/// A request with headers, and a body when it is a POST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// What a server answered: a success with the body's size, or another status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok(u64),
    Other(u16),
}

impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            body: Some(body),
            ..Self::get(url)
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }
}

/// HTTP and HTTPS, with the system's certificate store and its proxy variables.
pub struct Http {
    agent: Agent,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Http {
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .max_redirects(5)
            .user_agent(concat!("uxdroid/", env!("CARGO_PKG_VERSION")))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Self {
            agent: config.into(),
        }
    }
}

/// The slowest transfer accepted, 1 Mbit/s: a download of `limit` bytes, and anything a server does
/// before it, must finish within 10 s plus `limit` at this rate, so a stalled or lying server holds
/// a command, and the lock of the source it refreshes, for a bounded time. F-Droid's whole index
/// gets 9 minutes, its entry 12 s.
const MIN_BYTES_PER_SECOND: u64 = 128 * 1024;
const MAX_TIME: Duration = Duration::from_secs(60 * 60);

pub(crate) fn time_for(limit: u64) -> Duration {
    (Duration::from_secs(10) + Duration::from_secs(limit / MIN_BYTES_PER_SECOND)).min(MAX_TIME)
}

fn timed_out(url: &str, budget: Duration) -> Error {
    Error::new(format!(
        "cannot fetch {url}: it did not finish within {} s, so it was stopped",
        budget.as_secs()
    ))
}

fn fetch_error(url: &str, budget: Duration, error: ureq::Error) -> Error {
    match error {
        ureq::Error::StatusCode(code) => {
            Error::new(format!("cannot fetch {url}: the server answered {code}"))
        }
        ureq::Error::Timeout(_) => timed_out(url, budget),
        ureq::Error::RequireHttpsOnly(_) => Error::new(format!(
            "cannot fetch {url}: it redirects to an address that is not https, which is refused"
        )),
        ureq::Error::TooManyRedirects => Error::new(format!(
            "cannot fetch {url}: it redirects, and a plain http address may not"
        )),
        other => Error::new(format!("cannot fetch {url}: {other}")),
    }
}

fn check_scheme(url: &str) -> Result<(), Error> {
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(())
    } else {
        Err(Error::new(format!(
            "cannot fetch {url}: only http and https are supported"
        )))
    }
}

impl Fetch for Http {
    fn copy(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, Error> {
        check_scheme(url)?;
        // An https address never leads to http, and plain http, allowed only on this machine,
        // follows no redirect at all.
        let https = url.starts_with("https://");
        let budget = time_for(limit);
        let response = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(budget))
            .https_only(https)
            .max_redirects(if https { 5 } else { 0 })
            .build()
            .call()
            .map_err(|error| fetch_error(url, budget, error))?;
        read_body(url, budget, response, limit, sink)
    }

    fn send(&self, request: &Request, limit: u64, sink: &mut dyn Write) -> Result<Status, Error> {
        let url = request.url.as_str();
        check_scheme(url)?;
        let https = url.starts_with("https://");
        let budget = time_for(limit);
        macro_rules! configured {
            ($builder:expr) => {{
                let mut builder = $builder;
                for (name, value) in &request.headers {
                    builder = builder.header(name, value);
                }
                builder
                    .config()
                    .timeout_global(Some(budget))
                    .https_only(https)
                    .max_redirects(if https { 5 } else { 0 })
                    .build()
            }};
        }
        let sent = match &request.body {
            Some(body) => configured!(self.agent.post(url)).send(&body[..]),
            None => configured!(self.agent.get(url)).call(),
        };
        let response = match sent {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(code)) => return Ok(Status::Other(code)),
            Err(error) => return Err(fetch_error(url, budget, error)),
        };
        read_body(url, budget, response, limit, sink).map(Status::Ok)
    }
}

fn read_body(
    url: &str,
    budget: Duration,
    mut response: ureq::http::Response<ureq::Body>,
    limit: u64,
    sink: &mut dyn Write,
) -> Result<u64, Error> {
    // With no redirect to follow, ureq hands the redirect itself back.
    if response.status().is_redirection() {
        return Err(fetch_error(url, budget, ureq::Error::TooManyRedirects));
    }
    // The body is read one byte past the limit, so a larger one is refused without being read.
    let mut body = response
        .body_mut()
        .as_reader()
        .take(limit.saturating_add(1));
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = body.read(&mut buffer).map_err(|error| {
            match error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<ureq::Error>())
            {
                Some(ureq::Error::Timeout(_)) => timed_out(url, budget),
                _ => Error::new(format!("cannot fetch {url}: {error}")),
            }
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            return Err(Error::new(format!(
                "{url} is larger than the {limit} bytes allowed, so it is refused"
            )));
        }
        sink.write_all(&buffer[..read])
            .map_err(|error| Error::new(format!("cannot store {url}: {error}")))?;
    }
    Ok(total)
}
