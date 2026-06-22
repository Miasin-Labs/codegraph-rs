/// Parse error with position info.
#[derive(Debug, thiserror::Error)]
#[error("parse error at position {position}: {message}")]
pub struct ParseError {
    pub position: usize,
    pub message: String,
}

impl ParseError {
    pub(super) fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}
