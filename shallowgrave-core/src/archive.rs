use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{Result, ShallowgraveError};
use crate::git;
use crate::hash::sha256_bytes;
use crate::manifest::{
    ArtefactInfo, BuildInfo, LifecycleState, LifecycleTimestamps, Manifest, SourceInfo, SCHEMA_V1,
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
    /// Directory to inspect for git provenance; typically the project's working dir.
    pub source_dir: &'a Path,
}

pub struct VerificationResult {
    pub application: String,
    pub version: String,
    pub artefact_name: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub ok: bool,
}

fn build_fingerprint(source: &SourceInfo, build: &BuildInfo) -> String {
    let canonical = format!(
        "commit={}\ncompiler={}\ncompiler_version={}\ntarget={}\nprofile={}\nfeatures={}",
        source.commit.as_deref().unwrap_or(""),
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

    pub fn archive(&self, req: ArchiveRequest) -> Result<Manifest> {
        let digest = self.store.put(req.binary_path)?;
        let size = fs::metadata(req.binary_path)?.len();
        let artefact_name = req
            .artefact_name
            .map(str::to_string)
            .unwrap_or_else(|| req.application.to_string());

        let source = git::source_info(req.source_dir);
        let mut build = BuildInfo {
            fingerprint: String::new(),
            compiler: req.compiler,
            compiler_version: req.compiler_version,
            target: req.target,
            profile: req.profile,
            features: req.features,
        };
        build.fingerprint = build_fingerprint(&source, &build);

        let manifest = Manifest {
            schema: SCHEMA_V1.to_string(),
            application: req.application.to_string(),
            version: req.version.to_string(),
            status: LifecycleState::Released,
            source,
            build,
            artefacts: vec![ArtefactInfo {
                name: artefact_name,
                artefact_type: "binary".to_string(),
                sha256: digest,
                size,
            }],
            lifecycle: LifecycleTimestamps {
                released: Some(Utc::now()),
                deprecated: None,
                retired: None,
            },
        };

        let dir = self.manifest_dir(req.application);
        fs::create_dir_all(&dir)?;
        let path = self.manifest_path(req.application, req.version);
        fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;

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

    pub fn inspect(&self, application: &str, version: &str) -> Result<Manifest> {
        let path = self.manifest_path(application, version);
        if !path.exists() {
            return Err(ShallowgraveError::ReleaseNotFound(
                application.to_string(),
                version.to_string(),
            ));
        }
        let text = fs::read_to_string(&path)?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|e| ShallowgraveError::CorruptManifest(path.clone(), e.to_string()))?;
        if manifest.schema != SCHEMA_V1 {
            return Err(ShallowgraveError::UnknownSchema(manifest.schema));
        }
        Ok(manifest)
    }

    pub fn verify(&self, application: &str, version: &str) -> Result<VerificationResult> {
        let manifest = self.inspect(application, version)?;
        let artefact = manifest
            .primary_artefact()
            .ok_or_else(|| ShallowgraveError::ReleaseNotFound(application.to_string(), version.to_string()))?;

        let actual = match self.store.verify(&artefact.sha256) {
            Ok(true) => Some(artefact.sha256.clone()),
            Ok(false) => {
                // Digest recomputation disagreed with the stored key: surface what we actually found.
                None
            }
            Err(ShallowgraveError::ArtefactNotFound(_)) => None,
            Err(e) => return Err(e),
        };

        Ok(VerificationResult {
            application: application.to_string(),
            version: version.to_string(),
            artefact_name: artefact.name.clone(),
            expected_sha256: artefact.sha256.clone(),
            ok: actual.is_some(),
            actual_sha256: actual,
        })
    }

    pub fn resurrect(&self, application: &str, version: &str, dest: &Path) -> Result<Manifest> {
        let manifest = self.inspect(application, version)?;
        let artefact = manifest
            .primary_artefact()
            .ok_or_else(|| ShallowgraveError::ReleaseNotFound(application.to_string(), version.to_string()))?;

        self.store.get(&artefact.sha256, dest)?;

        let actual = crate::hash::sha256_file(dest)?;
        if actual != artefact.sha256 {
            return Err(ShallowgraveError::DigestMismatch {
                expected: artefact.sha256.clone(),
                found: actual,
            });
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
}
