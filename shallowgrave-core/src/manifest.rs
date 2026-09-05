use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCHEMA_V1: &str = "shallowgrave/v1";
pub const SCHEMA_V2: &str = "shallowgrave/v2";

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

impl LifecycleState {
    /// Forward order per §10. Not strictly enforced (an operator may need to
    /// jump straight to Retired), but used to warn on backward transitions.
    fn rank(self) -> u8 {
        match self {
            LifecycleState::Development => 0,
            LifecycleState::Released => 1,
            LifecycleState::Maintained => 2,
            LifecycleState::Deprecated => 3,
            LifecycleState::Archived => 4,
            LifecycleState::Retired => 5,
        }
    }

    pub fn is_backward_transition(self, to: LifecycleState) -> bool {
        to.rank() < self.rank()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceInfo {
    pub repository: Option<String>,
    pub commit: Option<String>,
    /// True when the working tree had uncommitted changes at archive time,
    /// per the NON_REPRODUCIBLE_SOURCE_STATE policy in the directive (§32).
    pub dirty: bool,
    /// Hash over the UNI-recorded source tree (path+per-file sha256), when a
    /// `.uni/snapshot-v1.json` was available at archive time. Fills the
    /// `source_tree` component of the build fingerprint described in §5.
    #[serde(default)]
    pub source_tree_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Best-effort build fingerprint over the fields captured below. Does not
    /// yet incorporate a lockfile hash or full dependency resolution as the
    /// complete spec (§5) describes.
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

/// One platform-specific build within a release (§4: Release -> Build -> Binary).
/// A release accumulates one `BuildRecord` per distinct build fingerprint, so
/// multi-platform artefacts and rebuilds of the same target both stay
/// separately identified rather than overwriting each other (§9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRecord {
    pub build: BuildInfo,
    pub artefacts: Vec<ArtefactInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleTimestamps {
    pub released: Option<DateTime<Utc>>,
    pub deprecated: Option<DateTime<Utc>>,
    pub archived: Option<DateTime<Utc>>,
    pub retired: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub application: String,
    pub version: String,
    pub status: LifecycleState,
    pub source: SourceInfo,
    pub builds: Vec<BuildRecord>,
    pub lifecycle: LifecycleTimestamps,
}

impl Manifest {
    pub fn primary_artefact(&self) -> Option<&ArtefactInfo> {
        self.builds.first().and_then(|b| b.artefacts.first())
    }

    pub fn build_for_target(&self, target: &str) -> Option<&BuildRecord> {
        self.builds.iter().find(|b| b.build.target.as_deref() == Some(target))
    }

    pub fn artefact_for_target(&self, target: &str) -> Option<&ArtefactInfo> {
        self.build_for_target(target).and_then(|b| b.artefacts.first())
    }

    pub fn targets(&self) -> Vec<&str> {
        self.builds.iter().filter_map(|b| b.build.target.as_deref()).collect()
    }

    /// Parses either the current schema or a v1 manifest (single implicit
    /// build/artefact list), upgrading v1 in memory. Per §28, old manifests
    /// must remain readable without depending on which schema the running
    /// binary currently writes.
    pub fn from_json(text: &str) -> serde_json::Result<Manifest> {
        #[derive(Deserialize)]
        struct Tagged {
            schema: String,
        }
        let tagged: Tagged = serde_json::from_str(text)?;
        match tagged.schema.as_str() {
            SCHEMA_V2 => serde_json::from_str::<Manifest>(text),
            SCHEMA_V1 => {
                let v1: ManifestV1 = serde_json::from_str(text)?;
                Ok(v1.into())
            }
            other => Err(serde::de::Error::custom(format!("unsupported manifest schema: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestV1 {
    application: String,
    version: String,
    status: LifecycleState,
    #[serde(default)]
    source: SourceInfo,
    #[serde(default)]
    build: BuildInfo,
    #[serde(default)]
    artefacts: Vec<ArtefactInfo>,
    #[serde(default)]
    lifecycle: LifecycleTimestamps,
}

impl From<ManifestV1> for Manifest {
    fn from(v1: ManifestV1) -> Self {
        Manifest {
            schema: SCHEMA_V2.to_string(),
            application: v1.application,
            version: v1.version,
            status: v1.status,
            source: v1.source,
            builds: vec![BuildRecord { build: v1.build, artefacts: v1.artefacts }],
            lifecycle: v1.lifecycle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_manifest_reads_and_upgrades() {
        let v1_json = r#"{
            "schema": "shallowgrave/v1",
            "application": "uni",
            "version": "1.4.2",
            "status": "released",
            "source": {"repository": null, "commit": "abc123", "dirty": false},
            "build": {"fingerprint": "f", "compiler": "rustc", "compiler_version": null, "target": "x86_64-unknown-linux-gnu", "profile": "release", "features": []},
            "artefacts": [{"name": "uni", "type": "binary", "sha256": "deadbeef", "size": 42}],
            "lifecycle": {"released": null, "deprecated": null, "retired": null}
        }"#;
        let manifest = Manifest::from_json(v1_json).unwrap();
        assert_eq!(manifest.schema, SCHEMA_V2);
        assert_eq!(manifest.builds.len(), 1);
        assert_eq!(manifest.primary_artefact().unwrap().sha256, "deadbeef");
        assert_eq!(manifest.artefact_for_target("x86_64-unknown-linux-gnu").unwrap().sha256, "deadbeef");
    }

    #[test]
    fn backward_transition_detected() {
        assert!(LifecycleState::Retired.is_backward_transition(LifecycleState::Released));
        assert!(!LifecycleState::Released.is_backward_transition(LifecycleState::Deprecated));
    }
}
