//! Review Gate: classify a pull request into the reviewer types it actually needs.

pub mod classifier;
pub mod config;
pub mod diff;
pub mod engine;
pub mod report;

/// Version of the JSON result document.
pub const SCHEMA_VERSION: &str = "1.0";

/// Expected, user-facing failures.
#[derive(Debug)]
pub enum Error {
    /// The rule configuration is missing or invalid.
    Config(String),
    /// The diff could not be read or parsed.
    Diff(String),
    /// The classification backend could not answer.
    Model(String),
    /// Reading or writing a file failed.
    Io(String),
}

impl Error {
    /// Process exit code for this failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) => 2,
            _ => 1,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Config(message)
            | Error::Diff(message)
            | Error::Model(message)
            | Error::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;
