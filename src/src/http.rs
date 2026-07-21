//! Minimal HTTP abstraction so network sources are mockable in tests.

use crate::error::{Error, Result};

/// Outcome of a GET request. 404 is reported separately because it is a
/// normal "not yet archived" answer from the Arch Archive, not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    Ok(String),
    NotFound,
}

pub trait HttpClient {
    fn get(&self, url: &str) -> Result<FetchOutcome>;
}

/// Blocking HTTP client with a bounded timeout and an identifying
/// User-Agent (politeness toward archive.archlinux.org and the AUR).
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .user_agent(concat!(
                "pactience/",
                env!("CARGO_PKG_VERSION"),
                " (+",
                env!("CARGO_PKG_REPOSITORY"),
                ")"
            ))
            .build();
        UreqClient {
            agent: config.into(),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqClient {
    fn get(&self, url: &str) -> Result<FetchOutcome> {
        match self.agent.get(url).call() {
            Ok(mut response) => {
                let body = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| Error::http(url, e.to_string()))?;
                Ok(FetchOutcome::Ok(body))
            }
            Err(ureq::Error::StatusCode(404)) => Ok(FetchOutcome::NotFound),
            Err(e) => Err(Error::http(url, e.to_string())),
        }
    }
}
