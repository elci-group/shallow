pub mod archive;
pub mod error;
pub mod git;
pub mod hash;
pub mod manifest;
pub mod store;

pub use archive::{Archive, ArchiveRequest, VerificationResult};
pub use error::{Result, ShallowgraveError};
pub use manifest::{ArtefactInfo, BuildInfo, LifecycleState, Manifest, SourceInfo};
