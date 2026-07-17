use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("page {0} not found in store")]
    PageNotFound(u64),

    #[error("page is full, cannot insert cell")]
    PageFull,

    #[error("tuple size {0} exceeds maximum allowed size")]
    TupleTooLarge(usize),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("corrupt page detected: {0}")]
    CorruptPage(String),

    #[error("replacer either empty, or every tracked page currently pinned")]
    LruEviction,
}
