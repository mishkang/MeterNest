use serde::Serialize;

/// Mirrors the JS errors thrown through Electron IPC: the renderer only ever
/// reads `error.message`, so we serialize to the plain message string.
#[derive(Debug, Clone)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub status: Option<i64>,
    pub account_identity: Option<String>,
}

impl AppError {
    pub fn new(code: &str, message: &str) -> Self {
        Self { code: code.to_string(), message: message.to_string(), status: None, account_identity: None }
    }
    pub fn with_status(code: &str, message: &str, status: i64) -> Self {
        Self { status: Some(status), ..Self::new(code, message) }
    }
    pub fn message(message: impl Into<String>) -> Self {
        Self { code: String::new(), message: message.into(), status: None, account_identity: None }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("message", &self.message)?;
        state.end()
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        AppError::message(error.to_string())
    }
}
