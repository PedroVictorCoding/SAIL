use thiserror::Error;

pub type Result<T> = std::result::Result<T, SailError>;

#[derive(Error, Debug)]
pub enum SailError {
    #[error("Subsystem error [{subsystem}]: {message}")]
    Subsystem { subsystem: String, message: String },

    #[error("Event bus channel closed or lagged: {0}")]
    Channel(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Inference failure: {0}")]
    Inference(String),

    #[error("Memory storage failure: {0}")]
    Memory(String),

    #[error("Tool execution failed: {0}")]
    ToolExecution(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),
}
