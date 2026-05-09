#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    General(String),
}

impl AppError {
    pub fn msg(s: impl Into<String>) -> Self {
        AppError::General(s.into())
    }
}

pub type AppResult<T> = Result<T, AppError>;
