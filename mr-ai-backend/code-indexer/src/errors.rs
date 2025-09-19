use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("utf8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("serde json error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("tree-sitter language error")]
    TreeSitterLanguage,

    #[error("tree-sitter parse error")]
    TreeSitterParse,

    #[error("invalid state: {0}")]
    InvalidState(&'static str),

    #[error("lsp protocol error: {0}")]
    LspProtocol(&'static str),

    #[error("process spawn error: {0}")]
    Spawn(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
