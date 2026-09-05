use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ShallowgraveError {
    #[error("source artefact not found: {0}")]
    ArtefactSourceNotFound(PathBuf),
    #[error("release not found in archive: {0}@{1}")]
    ReleaseNotFound(String, String),
    #[error("no archived releases found for application: {0}")]
    ApplicationNotFound(String),
    #[error("artefact bytes missing from object store for digest {0}")]
    ArtefactNotFound(String),
    #[error("digest mismatch: expected {expected}, found {found}")]
    DigestMismatch { expected: String, found: String },
    #[error("corrupt manifest at {0}: {1}")]
    CorruptManifest(PathBuf, String),
    #[error("unsupported manifest schema: {0}")]
    UnknownSchema(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ShallowgraveError>;
