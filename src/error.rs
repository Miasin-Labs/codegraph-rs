//! CodeGraph Error Types
//!
//! Ported from `src/errors.ts`. The TS class hierarchy collapses into a single
//! `CodeGraphError` enum; `code()` reproduces the TS `error.code` strings.

use std::sync::RwLock;

/// Convenient result alias used across the crate.
pub type Result<T, E = CodeGraphError> = std::result::Result<T, E>;

/// All CodeGraph-specific errors (mirrors the TS CodeGraphError class family).
#[derive(Debug, thiserror::Error)]
pub enum CodeGraphError {
    /// Error reading or accessing files (TS: FileError)
    #[error("{message} (file: {file_path})")]
    File { message: String, file_path: String },

    /// Error parsing source code (TS: ParseError)
    #[error("{message} (file: {file_path})")]
    Parse {
        message: String,
        file_path: String,
        line: Option<u32>,
        column: Option<u32>,
    },

    /// Error with database operations (TS: DatabaseError)
    #[error("{message} (operation: {operation})")]
    Database { message: String, operation: String },

    /// Error with search operations (TS: SearchError)
    #[error("{message} (query: {query})")]
    Search { message: String, query: String },

    /// Error with vector/embedding operations (TS: VectorError)
    #[error("{message} (operation: {operation})")]
    Vector { message: String, operation: String },

    /// Error with configuration (TS: ConfigError)
    #[error("{message}")]
    Config { message: String },

    /// Underlying I/O error
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Underlying SQLite error
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// Underlying JSON error
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// Generic error with message (catch-all, mirrors plain `Error` in TS)
    #[error("{0}")]
    Other(String),
}

impl CodeGraphError {
    /// Error code for categorization — same strings as the TS `code` field.
    pub fn code(&self) -> &'static str {
        match self {
            CodeGraphError::File { .. } => "FILE_ERROR",
            CodeGraphError::Parse { .. } => "PARSE_ERROR",
            CodeGraphError::Database { .. } => "DATABASE_ERROR",
            CodeGraphError::Search { .. } => "SEARCH_ERROR",
            CodeGraphError::Vector { .. } => "VECTOR_ERROR",
            CodeGraphError::Config { .. } => "CONFIG_ERROR",
            CodeGraphError::Io(_) => "FILE_ERROR",
            CodeGraphError::Sqlite(_) => "DATABASE_ERROR",
            CodeGraphError::Json(_) => "CONFIG_ERROR",
            CodeGraphError::Other(_) => "ERROR",
        }
    }

    pub fn file(message: impl Into<String>, file_path: impl Into<String>) -> Self {
        CodeGraphError::File {
            message: message.into(),
            file_path: file_path.into(),
        }
    }

    pub fn parse(message: impl Into<String>, file_path: impl Into<String>) -> Self {
        CodeGraphError::Parse {
            message: message.into(),
            file_path: file_path.into(),
            line: None,
            column: None,
        }
    }

    pub fn database(message: impl Into<String>, operation: impl Into<String>) -> Self {
        CodeGraphError::Database {
            message: message.into(),
            operation: operation.into(),
        }
    }

    pub fn search(message: impl Into<String>, query: impl Into<String>) -> Self {
        CodeGraphError::Search {
            message: message.into(),
            query: query.into(),
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        CodeGraphError::Config {
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        CodeGraphError::Other(message.into())
    }
}

impl From<String> for CodeGraphError {
    fn from(s: String) -> Self {
        CodeGraphError::Other(s)
    }
}

impl From<&str> for CodeGraphError {
    fn from(s: &str) -> Self {
        CodeGraphError::Other(s.to_string())
    }
}

// =============================================================================
// Logging (mirrors Logger / setLogger / silentLogger in TS)
// =============================================================================

/// Simple logger for CodeGraph operations.
pub trait Logger: Send + Sync {
    fn debug(&self, message: &str, context: Option<&serde_json::Value>);
    fn warn(&self, message: &str, context: Option<&serde_json::Value>);
    fn error(&self, message: &str, context: Option<&serde_json::Value>);
}

/// Default console-based logger. Debug output is gated on `CODEGRAPH_DEBUG`.
pub struct DefaultLogger;

impl Logger for DefaultLogger {
    fn debug(&self, message: &str, context: Option<&serde_json::Value>) {
        if std::env::var_os("CODEGRAPH_DEBUG").is_some() {
            match context {
                Some(c) => eprintln!("[CodeGraph] {message} {c}"),
                None => eprintln!("[CodeGraph] {message}"),
            }
        }
    }
    fn warn(&self, message: &str, context: Option<&serde_json::Value>) {
        match context {
            Some(c) => eprintln!("[CodeGraph] {message} {c}"),
            None => eprintln!("[CodeGraph] {message}"),
        }
    }
    fn error(&self, message: &str, context: Option<&serde_json::Value>) {
        match context {
            Some(c) => eprintln!("[CodeGraph] {message} {c}"),
            None => eprintln!("[CodeGraph] {message}"),
        }
    }
}

/// Silent logger (no output) — useful for tests.
pub struct SilentLogger;

impl Logger for SilentLogger {
    fn debug(&self, _message: &str, _context: Option<&serde_json::Value>) {}
    fn warn(&self, _message: &str, _context: Option<&serde_json::Value>) {}
    fn error(&self, _message: &str, _context: Option<&serde_json::Value>) {}
}

static CURRENT_LOGGER: RwLock<Option<Box<dyn Logger>>> = RwLock::new(None);

/// Set the global logger.
pub fn set_logger(logger: Box<dyn Logger>) {
    *CURRENT_LOGGER.write().unwrap() = Some(logger);
}

/// Log a debug message via the global logger.
pub fn log_debug(message: &str, context: Option<&serde_json::Value>) {
    let guard = CURRENT_LOGGER.read().unwrap();
    match guard.as_ref() {
        Some(l) => l.debug(message, context),
        None => DefaultLogger.debug(message, context),
    }
}

/// Log a warning message via the global logger.
pub fn log_warn(message: &str, context: Option<&serde_json::Value>) {
    let guard = CURRENT_LOGGER.read().unwrap();
    match guard.as_ref() {
        Some(l) => l.warn(message, context),
        None => DefaultLogger.warn(message, context),
    }
}

/// Log an error message via the global logger.
pub fn log_error(message: &str, context: Option<&serde_json::Value>) {
    let guard = CURRENT_LOGGER.read().unwrap();
    match guard.as_ref() {
        Some(l) => l.error(message, context),
        None => DefaultLogger.error(message, context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_ts() {
        assert_eq!(CodeGraphError::file("x", "a.ts").code(), "FILE_ERROR");
        assert_eq!(CodeGraphError::parse("x", "a.ts").code(), "PARSE_ERROR");
        assert_eq!(
            CodeGraphError::database("x", "insert").code(),
            "DATABASE_ERROR"
        );
        assert_eq!(CodeGraphError::search("x", "q").code(), "SEARCH_ERROR");
        assert_eq!(CodeGraphError::config("x").code(), "CONFIG_ERROR");
    }
}
