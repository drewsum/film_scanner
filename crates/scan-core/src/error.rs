use thiserror::Error;

/// Errors produced by scanner backends and devices.
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("invalid scan request: {0}")]
    InvalidRequest(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("scan cancelled")]
    Cancelled,
    #[error("scanner backend error: {0}")]
    Backend(String),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience for attaching context when converting foreign errors into [`ScanError::Backend`].
pub trait ScanResultExt<T> {
    fn backend_context(self, context: &str) -> Result<T, ScanError>;
}

impl<T, E: std::fmt::Display> ScanResultExt<T> for Result<T, E> {
    fn backend_context(self, context: &str) -> Result<T, ScanError> {
        self.map_err(|e| ScanError::Backend(format!("{context}: {e}")))
    }
}
