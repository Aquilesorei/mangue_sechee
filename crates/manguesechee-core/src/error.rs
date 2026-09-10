use thiserror::Error;

#[derive(Debug, Error)]
pub enum MangueError {
    #[error("network error: {0}")]
    Network(String),
    #[error("input error: {0}")]
    Input(String),
    #[error("config error: {0}")]
    Config(String),
}
