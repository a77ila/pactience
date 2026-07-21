//! Central error type for the application.

use std::path::PathBuf;

/// All fallible operations in `pactience` funnel into this type so that
/// `main` can render a single, actionable error message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error in {path}: {message}")]
    Config { path: PathBuf, message: String },

    #[error("failed to run `{command}`: {message}")]
    Command { command: String, message: String },

    #[error("`{command}` exited with {status}: {stderr}")]
    CommandStatus {
        command: String,
        status: String,
        stderr: String,
    },

    #[error("network request to {url} failed: {message}")]
    Http { url: String, message: String },

    #[error("failed to parse {what}: {message}")]
    Parse { what: String, message: String },

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cache error: {0}")]
    Cache(String),
}

impl Error {
    pub fn config(path: PathBuf, message: impl Into<String>) -> Self {
        Error::Config {
            path,
            message: message.into(),
        }
    }

    pub fn command(command: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Command {
            command: command.into(),
            message: message.into(),
        }
    }

    pub fn http(url: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Http {
            url: url.into(),
            message: message.into(),
        }
    }

    pub fn parse(what: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Parse {
            what: what.into(),
            message: message.into(),
        }
    }

    pub fn io(path: PathBuf, source: std::io::Error) -> Self {
        Error::Io { path, source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
