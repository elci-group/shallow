use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCHEMA_V1: &str = "shallowgrave/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Development,
    Released,
    Maintained,
    Deprecated,
    Archived,
    Retired,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceInfo {
    pub repository: Option<String>,
    pub commit: Option<String>,
    /// True when the working tree had uncommitted changes at archive time,
    /// per the NON_REPRODUCIBLE_SOURCE_STATE policy in the directive (§32).
    pub dirty: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Best-effort build fingerprint. Currently hashes only the fields captured
    /// below (commit, compiler, target, profile, features) — it does not yet
    /// incorporate source-tree or lockfile hashes as the full spec (§5) calls for.
    pub fingerprint: String,
    pub compiler: Option<String>,
    pub compiler_version: Option<String>,
    pub target: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtefactInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub artefact_type: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleTimestamps {
    pub released: Option<DateTime<Utc>>,
    pub deprecated: Option<DateTime<Utc>>,
    pub retired: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub application: String,
    pub version: String,
    pub status: LifecycleState,
    pub source: SourceInfo,
    pub build: BuildInfo,
    pub artefacts: Vec<ArtefactInfo>,
    pub lifecycle: LifecycleTimestamps,
}

impl Manifest {
    pub fn primary_artefact(&self) -> Option<&ArtefactInfo> {
        self.artefacts.first()
    }
}
