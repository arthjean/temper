use std::fmt;

pub(crate) type Result<T> = std::result::Result<T, TemperError>;

#[derive(Debug)]
pub(crate) struct TemperError {
    message: String,
}

impl TemperError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TemperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TemperError {}
