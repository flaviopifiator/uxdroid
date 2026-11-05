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
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // The largest download allowed, an APK of 2 GiB, at about 2 MiB/s.
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
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

impl Fetch for Http {
    fn copy(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, Error> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(Error::new(format!(
                "cannot fetch {url}: only http and https are supported"
            )));
        }
        let mut response = self.agent.get(url).call().map_err(|error| match error {
            ureq::Error::StatusCode(code) => {
                Error::new(format!("cannot fetch {url}: the server answered {code}"))
            }
            other => Error::new(format!("cannot fetch {url}: {other}")),
        })?;
        // The body is read one byte past the limit, so a larger one is refused without being read.
        let mut body = response
            .body_mut()
            .as_reader()
            .take(limit.saturating_add(1));
        let mut buffer = vec![0; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = body
                .read(&mut buffer)
                .map_err(|error| Error::new(format!("cannot fetch {url}: {error}")))?;
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
}
