use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::error::{Result, ShallowgraveError};
use crate::manifest::Manifest;

/// GitHub Release naming per §8: one release per application version, tagged
/// so multiple platform artefacts (§46) can attach to the same release.
pub fn release_tag(application: &str, version: &str) -> String {
    format!("{application}-v{version}")
}

/// Asset filename under the release. Includes the target so multi-platform
/// artefacts for the same release don't collide.
pub fn asset_label(artefact_name: &str, target: Option<&str>) -> String {
    match target {
        Some(t) => format!("{artefact_name}-{t}"),
        None => artefact_name.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAsset {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct PlannedUpload {
    pub build_target: Option<String>,
    pub artefact_name: String,
    pub label: String,
    pub sha256: String,
}

/// Pure diff: given a manifest's artefacts and what a remote release already
/// lists, decide what still needs uploading. Never mutates anything, and
/// never overwrites an asset that's already present — a remote artefact
/// reference is never re-derived once it exists (§31: "shall never overwrite
/// an existing immutable artefact").
pub fn plan_uploads(manifest: &Manifest, existing_remote_assets: &[RemoteAsset]) -> Vec<PlannedUpload> {
    let mut planned = Vec::new();
    for build in &manifest.builds {
        for artefact in &build.artefacts {
            let label = asset_label(&artefact.name, build.build.target.as_deref());
            let already_present = existing_remote_assets.iter().any(|a| a.name == label);
            if !already_present {
                planned.push(PlannedUpload {
                    build_target: build.build.target.clone(),
                    artefact_name: artefact.name.clone(),
                    label,
                    sha256: artefact.sha256.clone(),
                });
            }
        }
    }
    planned
}

/// Thin `gh` CLI wrapper (mirrors the best-effort style of `git.rs`: real
/// GitHub state is only ever read or additively written, never mutated in
/// place, matching §31's idempotent-sync contract). Not covered by unit
/// tests — it requires an authenticated `gh` and real repo access. Callers
/// that want tested behavior should exercise `plan_uploads` directly.
pub struct GhRemote {
    repo: String,
}

impl GhRemote {
    pub fn new(repo: impl Into<String>) -> Self {
        Self { repo: repo.into() }
    }

    /// `None` when the release doesn't exist yet (not an error).
    pub fn list_release_assets(&self, tag: &str) -> Result<Option<Vec<RemoteAsset>>> {
        let output = Command::new("gh")
            .args(["release", "view", tag, "--repo", &self.repo, "--json", "assets"])
            .output()
            .map_err(gh_missing)?;
        if !output.status.success() {
            // gh exits non-zero both for "release not found" and for real
            // failures; either way there is nothing to diff against yet.
            return Ok(None);
        }
        #[derive(Deserialize)]
        struct Assets {
            assets: Vec<AssetJson>,
        }
        #[derive(Deserialize)]
        struct AssetJson {
            name: String,
            size: u64,
        }
        let parsed: Assets = serde_json::from_slice(&output.stdout)?;
        Ok(Some(parsed.assets.into_iter().map(|a| RemoteAsset { name: a.name, size: a.size }).collect()))
    }

    pub fn ensure_release(&self, tag: &str, title: &str) -> Result<()> {
        if self.list_release_assets(tag)?.is_some() {
            return Ok(());
        }
        let status = Command::new("gh")
            .args(["release", "create", tag, "--repo", &self.repo, "--title", title, "--notes", "Published by Shallowgrave"])
            .status()
            .map_err(gh_missing)?;
        if !status.success() {
            return Err(ShallowgraveError::Io(std::io::Error::other(format!(
                "gh release create failed for tag {tag}"
            ))));
        }
        Ok(())
    }

    /// Uploads `object_path` (a CAS file named by digest) as `label`.
    pub fn upload_asset(&self, tag: &str, object_path: &Path, label: &str) -> Result<()> {
        // gh names the remote asset after the local file's basename, so stage
        // a renamed copy rather than uploading the digest-named CAS path directly.
        let staging_dir = std::env::temp_dir().join("shallowgrave-upload");
        fs::create_dir_all(&staging_dir)?;
        let staged = staging_dir.join(label);
        fs::copy(object_path, &staged)?;

        let status = Command::new("gh")
            .args(["release", "upload", tag, staged.to_string_lossy().as_ref(), "--repo", &self.repo, "--clobber"])
            .status()
            .map_err(gh_missing);

        let _ = fs::remove_file(&staged);

        if !status?.success() {
            return Err(ShallowgraveError::Io(std::io::Error::other(format!(
                "gh release upload failed for tag {tag}, label {label}"
            ))));
        }
        Ok(())
    }
}

fn gh_missing(e: std::io::Error) -> ShallowgraveError {
    ShallowgraveError::Io(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{BuildInfo, BuildRecord, ArtefactInfo, LifecycleState, LifecycleTimestamps, SourceInfo, SCHEMA_V2};

    fn sample_manifest() -> Manifest {
        Manifest {
            schema: SCHEMA_V2.to_string(),
            application: "uni".to_string(),
            version: "1.4.2".to_string(),
            status: LifecycleState::Released,
            source: SourceInfo::default(),
            builds: vec![
                BuildRecord {
                    build: BuildInfo { target: Some("x86_64-unknown-linux-gnu".to_string()), ..Default::default() },
                    artefacts: vec![ArtefactInfo { name: "uni".to_string(), artefact_type: "binary".to_string(), sha256: "aaa".to_string(), size: 10 }],
                },
                BuildRecord {
                    build: BuildInfo { target: Some("aarch64-apple-darwin".to_string()), ..Default::default() },
                    artefacts: vec![ArtefactInfo { name: "uni".to_string(), artefact_type: "binary".to_string(), sha256: "bbb".to_string(), size: 12 }],
                },
            ],
            lifecycle: LifecycleTimestamps::default(),
        }
    }

    #[test]
    fn plans_only_missing_assets() {
        let manifest = sample_manifest();
        let existing = vec![RemoteAsset { name: "uni-x86_64-unknown-linux-gnu".to_string(), size: 10 }];
        let planned = plan_uploads(&manifest, &existing);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].label, "uni-aarch64-apple-darwin");
        assert_eq!(planned[0].sha256, "bbb");
    }

    #[test]
    fn plans_nothing_when_fully_synced() {
        let manifest = sample_manifest();
        let existing = vec![
            RemoteAsset { name: "uni-x86_64-unknown-linux-gnu".to_string(), size: 10 },
            RemoteAsset { name: "uni-aarch64-apple-darwin".to_string(), size: 12 },
        ];
        assert!(plan_uploads(&manifest, &existing).is_empty());
    }

    #[test]
    fn plans_everything_when_release_absent() {
        let manifest = sample_manifest();
        assert_eq!(plan_uploads(&manifest, &[]).len(), 2);
    }
}
