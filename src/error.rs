#[derive(Debug, thiserror::Error)]
pub enum GmError {
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("font not found: {0} (pass --fonts with the same files used for dict build)")]
    FontNotFound(String),
    #[error("font hash mismatch: {0} (pass --fonts with the exact files used for dict build)")]
    FontHashMismatch(String),
    #[error("dictionary is incompatible: {0}")]
    DictIncompatible(String),
}
