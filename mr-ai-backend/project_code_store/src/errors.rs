use thiserror::Error;

pub type Result<T> = std::result::Result<T, GitCloneError>;

#[derive(Debug, Error)]
pub enum GitCloneError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("task join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("git error: {0}")]
    Git(#[from] git2::Error),
}
