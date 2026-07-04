use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Page {0} not found in store")]
    PageNotFound(u64),

    #[error("Page is full, cannot insert cell")]
    PageFull,

    #[error("Tuple size {0} exceeds maximum allowed size")]
    TupleTooLarge(usize),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Corrupt page detected: {0}")]
    CorruptPage(String),
}
