use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("cache error: {0}")]
    Cache(String),
    #[error("pack error: {0}")]
    Pack(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("http error: {0}")]
    Http(String),
}

pub type Result<T> = std::result::Result<T, Error>;
