use std::{io, path::PathBuf};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, WorkspaceError>;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("this command is only supported on macOS")]
    UnsupportedPlatform,

    #[error("invalid workspace name '{0}'; use letters, numbers, dots, dashes, and underscores")]
    InvalidName(String),

    #[error("workspace '{0}' was not found")]
    NotFound(String),

    #[error("workspace '{0}' already exists; pass --force to overwrite")]
    AlreadyExists(String),

    #[error("could not create workspace data directory at {path}: {source}")]
    CreateDataDir { path: PathBuf, source: io::Error },

    #[error("could not read {path}: {source}")]
    ReadFile { path: PathBuf, source: io::Error },

    #[error("could not write {path}: {source}")]
    WriteFile { path: PathBuf, source: io::Error },

    #[error("could not delete {path}: {source}")]
    DeleteFile { path: PathBuf, source: io::Error },

    #[error("could not parse snapshot JSON at {path}: {source}")]
    ParseSnapshot {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("could not encode snapshot JSON: {0}")]
    EncodeSnapshot(#[from] serde_json::Error),

    #[error("interactive configuration failed: {0}")]
    Interaction(String),

    #[error("macOS API call failed: {0}")]
    MacOs(String),

    #[error("Accessibility permission is required to restore windows. Grant access in System Settings > Privacy & Security > Accessibility, then rerun restore.")]
    AccessibilityPermissionRequired,
}

impl WorkspaceError {
    pub fn exit_code(&self) -> u8 {
        match self {
            WorkspaceError::InvalidName(_) => 2,
            WorkspaceError::NotFound(_) => 3,
            WorkspaceError::AlreadyExists(_) => 4,
            WorkspaceError::AccessibilityPermissionRequired => 5,
            WorkspaceError::UnsupportedPlatform => 6,
            _ => 1,
        }
    }
}
