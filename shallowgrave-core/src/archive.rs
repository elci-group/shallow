use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{Result, ShallowgraveError};
use crate::git;
use crate::hash::sha256_bytes;
use crate::integrations::uni;
use crate::manifest::{
    ArtefactInfo, BuildInfo, BuildRecord, LifecycleState, LifecycleTimestamps, Manifest, SourceInfo,
    SCHEMA_V2,
};
use crate::store::{ArtifactStore, LocalStore};

/// Root of a local Shallowgrave archive: manifests (the authoritative index)
/// plus the content-addressable object store (§7).
pub struct Archive {
    root: PathBuf,
    store: LocalStore,
}

pub struct ArchiveRequest<'a> {
    pub application: &'a str,
    pub version: &'a str,
    pub binary_path: &'a Path,
    pub artefact_name: Option<&'a str>,
    pub target: Option<String>,
    pub profile: Option<String>,
    pub features: Vec<String>,
    pub compiler: Option<String>,
    pub compiler_version: Option<String>,
    /// Directory to inspect for git/UNI provenance; typically the project's working dir.
    pub source_dir: &'a Path,
}

pub struct VerificationResult {
    pub application: String,
    pub version: String,
    pub target: Option<String>,
    pub artefact_name: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub ok: bool,
}

pub struct LifecycleTransition {
    pub manifest: Manifest,
    /// Set when the requested state is earlier than the current one in the
    /// §10 ordering (still applied — an operator may have a real reason).
    pub backward_warning: bool,
}

pub struct DoctorIssue {
    pub application: String,
    pub version: String,
    pub target: Option<String>,
    pub detail: String,
}

#[derive(Default)]
pub struct DoctorReport {
    pub applications: usize,
    pub releases: usize,
    pub artefact_entries: usize,
    pub unique_objects: usize,
    pub corrupt_manifests: Vec<PathBuf>,
    pub missing_artefacts: Vec<DoctorIssue>,
    pub digest_mismatches: Vec<DoctorIssue>,
    pub inconsistent_lifecycle: Vec<DoctorIssue>,
    pub orphaned_objects: Vec<String>,
}

impl DoctorReport {
    pub fn is_healthy(&self) -> bool {
        self.corrupt_manifests.is_empty()
            && self.missing_artefacts.is_empty()
            && self.digest_mismatches.is_empty()
            && self.inconsistent_lifecycle.is_empty()
    }

    pub fn health_score(&self) -> i64 {
        let mut score: i64 = 100;
        score -= 10 * self.corrupt_manifests.len() as i64;
        score -= 5 * self.missing_artefacts.len() as i64;
        score -= 10 * self.digest_mismatches.len() as i64;
        score -= 2 * self.inconsistent_lifecycle.len() as i64;
        score.clamp(0, 100)
    }
}

pub struct GcReport {
    pub removed_objects: Vec<String>,
    pub freed_bytes: u64,
}

fn build_fingerprint(source: &SourceInfo, build: &BuildInfo) -> String {
    let canonical = format!(
        "commit={}\nsource_tree={}\ncompiler={}\ncompiler_version={}\ntarget={}\nprofile={}\nfeatures={}",
        source.commit.as_deref().unwrap_or(""),
        source.source_tree_hash.as_deref().unwrap_or(""),
        build.compiler.as_deref().unwrap_or(""),
        build.compiler_version.as_deref().unwrap_or(""),
        build.target.as_deref().unwrap_or(""),
        build.profile.as_deref().unwrap_or(""),
        build.features.join(","),
    );
    sha256_bytes(canonical.as_bytes())
}

impl Archive {
    pub fn open(root: PathBuf) -> Result<Self> {
        let store = LocalStore::new(root.join("objects"))?;
        fs::create_dir_all(root.join("manifests"))?;
        Ok(Self { root, store })
    }

    fn manifest_dir(&self, application: &str) -> PathBuf {
        self.root.join("manifests").join(application)
    }

    fn manifest_path(&self, application: &str, version: &str) -> PathBuf {
        self.manifest_dir(application).join(format!("{version}.json"))
    }

    fn write_manifest(&self, manifest: &Manifest) -> Result<()> {
        let dir = self.manifest_dir(&manifest.application);
        fs::create_dir_all(&dir)?;
        let path = self.manifest_path(&manifest.application, &manifest.version);
        fs::write(&path, serde_json::to_string_pretty(manifest)?)?;
        Ok(())
    }

    /// Archives a binary as a release build. If a manifest already exists for
    /// this `app@version`, the artefact is merged into it: a build sharing the
    /// same fingerprint has its artefact list replaced (idempotent re-archive
    /// of the same build), otherwise a new `BuildRecord` is appended so
    /// multi-platform artefacts (§46) and distinct rebuilds stay separately
    /// identified rather than overwriting history (§9).
    pub fn archive(&self, req: ArchiveRequest) -> Result<Manifest> {
        let digest = self.store.put(req.binary_path)?;
        let size = fs::metadata(req.binary_path)?.len();
        let artefact_name = req
            .artefact_name
            .map(str::to_string)
            .unwrap_or_else(|| req.application.to_string());

        let mut source = git::source_info(req.source_dir);
        if let Some(snapshot) = uni::snapshot_info(req.source_dir) {
            source.source_tree_hash = Some(snapshot.source_tree_hash);
            if source.commit.is_none() {
                source.commit = snapshot.target_commit;
            }
        }

        let mut build = BuildInfo {
            fingerprint: String::new(),
            compiler: req.compiler,
            compiler_version: req.compiler_version,
            target: req.target,
            profile: req.profile,
            features: req.features,
        };
        build.fingerprint = build_fingerprint(&source, &build);

        let artefact =
            ArtefactInfo { name: artefact_name, artefact_type: "binary".to_string(), sha256: digest, size };

        let mut manifest = match self.inspect(req.application, req.version) {
            Ok(existing) => existing,
            Err(ShallowgraveError::ReleaseNotFound(_, _)) => Manifest {
                schema: SCHEMA_V2.to_string(),
                application: req.application.to_string(),
                version: req.version.to_string(),
                status: LifecycleState::Released,
                source: source.clone(),
                builds: Vec::new(),
                lifecycle: LifecycleTimestamps { released: Some(Utc::now()), ..Default::default() },
            },
            Err(e) => return Err(e),
        };
        manifest.source = source;

        match manifest.builds.iter_mut().find(|b| b.build.fingerprint == build.fingerprint) {
            Some(existing_build) => existing_build.artefacts = vec![artefact],
            None => manifest.builds.push(BuildRecord { build, artefacts: vec![artefact] }),
        }

        self.write_manifest(&manifest)?;
        Ok(manifest)
    }

    pub fn find(&self, application: &str) -> Result<Vec<String>> {
        let dir = self.manifest_dir(application);
        if !dir.exists() {
            return Err(ShallowgraveError::ApplicationNotFound(application.to_string()));
        }
        let mut versions = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                versions.push(stem.to_string());
            }
        }
        versions.sort();
        Ok(versions)
    }

    /// Every application with at least one archived release.
    pub fn applications(&self) -> Result<Vec<String>> {
        let dir = self.root.join("manifests");
        let mut apps = Vec::new();
        if !dir.exists() {
            return Ok(apps);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    apps.push(name.to_string());
                }
            }
        }
        apps.sort();
        Ok(apps)
    }

    pub fn inspect(&self, application: &str, version: &str) -> Result<Manifest> {
        let path = self.manifest_path(application, version);
        if !path.exists() {
            return Err(ShallowgraveError::ReleaseNotFound(
                application.to_string(),
                version.to_string(),
            ));
        }
        let text = fs::read_to_string(&path)?;
        Manifest::from_json(&text).map_err(|e| ShallowgraveError::CorruptManifest(path.clone(), e.to_string()))
    }

    fn select_artefact<'m>(&self, manifest: &'m Manifest, target: Option<&str>) -> Result<&'m ArtefactInfo> {
        match target {
            Some(t) => manifest.artefact_for_target(t).ok_or_else(|| {
                ShallowgraveError::ReleaseNotFound(manifest.application.clone(), format!("{}@{t}", manifest.version))
            }),
            None => manifest
                .primary_artefact()
                .ok_or_else(|| ShallowgraveError::ReleaseNotFound(manifest.application.clone(), manifest.version.clone())),
        }
    }

    pub fn verify(&self, application: &str, version: &str, target: Option<&str>) -> Result<VerificationResult> {
        let manifest = self.inspect(application, version)?;
        let artefact = self.select_artefact(&manifest, target)?;

        let actual = match self.store.verify(&artefact.sha256) {
            Ok(true) => Some(artefact.sha256.clone()),
            Ok(false) => None,
            Err(ShallowgraveError::ArtefactNotFound(_)) => None,
            Err(e) => return Err(e),
        };

        Ok(VerificationResult {
            application: application.to_string(),
            version: version.to_string(),
            target: target.map(str::to_string),
            artefact_name: artefact.name.clone(),
            expected_sha256: artefact.sha256.clone(),
            ok: actual.is_some(),
            actual_sha256: actual,
        })
    }

    /// Materializes an object by digest at `dest`, without manifest lookup or
    /// digest re-verification (callers such as `sync` that already know the
    /// digest from a manifest they hold). Prefer `resurrect` for user-facing restores.
    pub fn export_object(&self, digest: &str, dest: &Path) -> Result<()> {
        self.store.get(digest, dest)
    }

    pub fn resurrect(&self, application: &str, version: &str, target: Option<&str>, dest: &Path) -> Result<Manifest> {
        let manifest = self.inspect(application, version)?;
        let artefact = self.select_artefact(&manifest, target)?;

        self.store.get(&artefact.sha256, dest)?;

        let actual = crate::hash::sha256_file(dest)?;
        if actual != artefact.sha256 {
            return Err(ShallowgraveError::DigestMismatch { expected: artefact.sha256.clone(), found: actual });
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(dest)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(dest, perms)?;
        }

        Ok(manifest)
    }

    fn transition(&self, application: &str, version: &str, to: LifecycleState) -> Result<LifecycleTransition> {
        let mut manifest = self.inspect(application, version)?;
        let backward_warning = manifest.status.is_backward_transition(to);
        manifest.status = to;
        let now = Some(Utc::now());
        match to {
            LifecycleState::Deprecated => manifest.lifecycle.deprecated = now,
            LifecycleState::Archived => manifest.lifecycle.archived = now,
            LifecycleState::Retired => manifest.lifecycle.retired = now,
            _ => {}
        }
        self.write_manifest(&manifest)?;
        Ok(LifecycleTransition { manifest, backward_warning })
    }

    /// §10: ACTIVE -> DEPRECATED never deletes anything, only marks the release.
    pub fn deprecate(&self, application: &str, version: &str) -> Result<LifecycleTransition> {
        self.transition(application, version, LifecycleState::Deprecated)
    }

    pub fn retire(&self, application: &str, version: &str) -> Result<LifecycleTransition> {
        self.transition(application, version, LifecycleState::Retired)
    }

    /// Local-archive integrity audit (§30). Scoped to what's actually
    /// checkable without a configured remote: manifest parseability, artefact
    /// presence/digest correctness, lifecycle timestamp consistency, and
    /// objects no manifest currently references.
    pub fn doctor(&self) -> Result<DoctorReport> {
        let mut report = DoctorReport::default();
        let mut referenced_digests = std::collections::HashSet::new();

        let applications = self.applications()?;
        report.applications = applications.len();

        for app in &applications {
            let versions = match self.find(app) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for version in versions {
                let path = self.manifest_path(app, &version);
                let text = match fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(_) => {
                        report.corrupt_manifests.push(path);
                        continue;
                    }
                };
                let manifest = match Manifest::from_json(&text) {
                    Ok(m) => m,
                    Err(_) => {
                        report.corrupt_manifests.push(path);
                        continue;
                    }
                };
                report.releases += 1;

                let lifecycle_expected = match manifest.status {
                    LifecycleState::Deprecated => manifest.lifecycle.deprecated.is_some(),
                    LifecycleState::Archived => manifest.lifecycle.archived.is_some(),
                    LifecycleState::Retired => manifest.lifecycle.retired.is_some(),
                    _ => true,
                };
                if !lifecycle_expected {
                    report.inconsistent_lifecycle.push(DoctorIssue {
                        application: app.clone(),
                        version: version.clone(),
                        target: None,
                        detail: format!("status {:?} has no matching lifecycle timestamp", manifest.status),
                    });
                }

                for build in &manifest.builds {
                    for artefact in &build.artefacts {
                        report.artefact_entries += 1;
                        referenced_digests.insert(artefact.sha256.clone());

                        match self.store.verify(&artefact.sha256) {
                            Ok(true) => {}
                            Ok(false) => report.digest_mismatches.push(DoctorIssue {
                                application: app.clone(),
                                version: version.clone(),
                                target: build.build.target.clone(),
                                detail: format!("stored object for {} failed digest recomputation", artefact.sha256),
                            }),
                            Err(ShallowgraveError::ArtefactNotFound(_)) => {
                                report.missing_artefacts.push(DoctorIssue {
                                    application: app.clone(),
                                    version: version.clone(),
                                    target: build.build.target.clone(),
                                    detail: format!("object {} not found in store", artefact.sha256),
                                })
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        report.unique_objects = referenced_digests.len();

        let objects_dir = self.root.join("objects");
        if objects_dir.exists() {
            for prefix_entry in fs::read_dir(&objects_dir)? {
                let prefix_entry = prefix_entry?;
                if !prefix_entry.file_type()?.is_dir() {
                    continue;
                }
                for object_entry in fs::read_dir(prefix_entry.path())? {
                    let object_entry = object_entry?;
                    if let Some(name) = object_entry.file_name().to_str() {
                        if !referenced_digests.contains(name) {
                            report.orphaned_objects.push(name.to_string());
                        }
                    }
                }
            }
        }

        Ok(report)
    }

    /// Removes only objects no manifest currently references (§25: "garbage
    /// collect the working set, not the history"). Never touches an object a
    /// manifest still points to.
    pub fn gc_orphaned(&self) -> Result<GcReport> {
        let report = self.doctor()?;
        let objects_dir = self.root.join("objects");
        let mut removed = Vec::new();
        let mut freed_bytes = 0u64;

        for digest in report.orphaned_objects {
            let prefix = &digest[..2.min(digest.len())];
            let path = objects_dir.join(prefix).join(&digest);
            if let Ok(meta) = fs::metadata(&path) {
                freed_bytes += meta.len();
            }
            if fs::remove_file(&path).is_ok() {
                removed.push(digest);
            }
        }

        Ok(GcReport { removed_objects: removed, freed_bytes })
    }
}
