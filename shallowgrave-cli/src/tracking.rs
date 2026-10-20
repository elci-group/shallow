use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shallowgrave_core::hash::{sha256_bytes, sha256_file};
use shallowgrave_core::{git, integrations::uni};
use shallowgrave_core::{Archive, ArchiveRequest};

const TRACKING_SCHEMA: &str = "shallowgrave/tracking-v1";

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackingRegistry {
    #[serde(default = "tracking_schema")]
    pub schema: String,
    #[serde(default)]
    pub projects: Vec<TrackedProject>,
}

fn tracking_schema() -> String {
    TRACKING_SCHEMA.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedProject {
    pub source_dir: PathBuf,
    pub binary: Option<PathBuf>,
    pub app: Option<String>,
    pub target: Option<String>,
    pub profile: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub last_archived: Option<TrackedSnapshot>,
    #[serde(default)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedSnapshot {
    pub application: String,
    pub version: String,
    pub sha256: String,
    /// Fingerprint of provenance and build settings, so identical output from
    /// a new commit/toolchain/configuration is still recorded as a new build.
    #[serde(default)]
    pub build_context: String,
    pub archived_at: DateTime<Utc>,
}

pub struct DaemonGuard {
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackEventKind {
    Archived,
    Unchanged,
    Error,
}

#[derive(Debug, Serialize)]
pub struct TrackEvent {
    pub source_dir: PathBuf,
    pub application: Option<String>,
    pub version: Option<String>,
    pub kind: TrackEventKind,
    pub detail: String,
}

#[derive(Deserialize)]
struct CargoToml {
    package: RawCargoPackage,
}

#[derive(Deserialize)]
struct RawCargoPackage {
    name: String,
    version: toml::Value,
}

pub struct CargoPackage {
    pub name: String,
    pub version: String,
}

impl Default for TrackingRegistry {
    fn default() -> Self {
        Self {
            schema: tracking_schema(),
            projects: Vec::new(),
        }
    }
}

impl DaemonGuard {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match create_pid_file(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = fs::read_to_string(&path).unwrap_or_default();
                let live = holder.trim().parse::<u32>().ok().is_some_and(pid_is_live);
                if live {
                    return Err(anyhow!(
                        "another shallowgrave daemon is running (pid {})",
                        holder.trim()
                    ));
                }
                fs::remove_file(&path).with_context(|| {
                    format!(
                        "stale daemon pid file could not be removed: {}",
                        path.display()
                    )
                })?;
                create_pid_file(&path)?;
                Ok(Self { path })
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn create_pid_file(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()
}

#[cfg(target_os = "linux")]
fn pid_is_live(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn pid_is_live(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

// On platforms without a portable process-liveness check, preserving an
// existing guard is safer than risking two writers. The operator can remove a
// confirmed-stale daemon.pid explicitly.
#[cfg(not(unix))]
fn pid_is_live(_pid: u32) -> bool {
    true
}

impl TrackingRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read tracking registry {}", path.display()))?;
        let registry: Self = serde_json::from_str(&text)
            .with_context(|| format!("invalid tracking registry {}", path.display()))?;
        if registry.schema != TRACKING_SCHEMA {
            return Err(anyhow!(
                "unsupported tracking registry schema `{}`",
                registry.schema
            ));
        }
        Ok(registry)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to atomically update {}", path.display()))?;
        Ok(())
    }

    pub fn add(&mut self, project: TrackedProject) -> bool {
        if let Some(existing) = self
            .projects
            .iter_mut()
            .find(|p| p.source_dir == project.source_dir)
        {
            let last_archived = existing.last_archived.take();
            *existing = project;
            existing.last_archived = last_archived;
            false
        } else {
            self.projects.push(project);
            self.projects
                .sort_by(|a, b| a.source_dir.cmp(&b.source_dir));
            true
        }
    }

    pub fn remove(&mut self, source_dir: &Path) -> bool {
        let old_len = self.projects.len();
        self.projects
            .retain(|project| project.source_dir != source_dir);
        self.projects.len() != old_len
    }

    pub fn scan(&mut self, archive: &Archive) -> Vec<TrackEvent> {
        self.projects
            .iter_mut()
            .map(|project| scan_project(project, archive))
            .collect()
    }
}

impl TrackedProject {
    pub fn new(
        source_dir: &Path,
        binary: Option<PathBuf>,
        app: Option<String>,
        target: Option<String>,
        profile: String,
        features: Vec<String>,
    ) -> Result<Self> {
        let source_dir = source_dir.canonicalize().with_context(|| {
            format!(
                "tracked source directory does not exist: {}",
                source_dir.display()
            )
        })?;
        if !source_dir.is_dir() {
            return Err(anyhow!(
                "tracked source path is not a directory: {}",
                source_dir.display()
            ));
        }
        read_cargo_package(&source_dir)?;
        let binary = binary.map(|path| {
            if path.is_absolute() {
                path
            } else {
                source_dir.join(path)
            }
        });

        Ok(Self {
            source_dir,
            binary,
            app,
            target,
            profile,
            features,
            last_archived: None,
            last_checked_at: None,
            last_error: None,
        })
    }
}

pub fn read_cargo_package(source_dir: &Path) -> Result<CargoPackage> {
    let manifest_path = source_dir.join("Cargo.toml");
    let text = fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "tracked project needs a Cargo.toml at {}",
            manifest_path.display()
        )
    })?;
    let package = toml::from_str::<CargoToml>(&text)
        .with_context(|| {
            format!(
                "failed to read package metadata from {}",
                manifest_path.display()
            )
        })?
        .package;
    let version = match package.version.as_str() {
        Some(version) => version.to_string(),
        None if package
            .version
            .get("workspace")
            .and_then(toml::Value::as_bool)
            == Some(true) =>
        {
            workspace_package_version(source_dir)?
        }
        None => {
            return Err(anyhow!(
                "package.version in {} is not a string or workspace inheritance",
                manifest_path.display()
            ))
        }
    };
    Ok(CargoPackage {
        name: package.name,
        version,
    })
}

fn workspace_package_version(source_dir: &Path) -> Result<String> {
    for directory in source_dir.ancestors() {
        let candidate = directory.join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(document) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        if let Some(version) = document
            .get("workspace")
            .and_then(|value| value.get("package"))
            .and_then(|value| value.get("version"))
            .and_then(toml::Value::as_str)
        {
            return Ok(version.to_string());
        }
    }
    Err(anyhow!(
        "package inherits version from a workspace, but no workspace.package.version was found above {}",
        source_dir.display()
    ))
}

fn host_target() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .args(["-vV"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
}

fn rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

fn scan_project(project: &mut TrackedProject, archive: &Archive) -> TrackEvent {
    project.last_checked_at = Some(Utc::now());
    match try_scan_project(project, archive) {
        Ok(event) => {
            project.last_error = None;
            event
        }
        Err(error) => {
            let detail = format!("{error:#}");
            project.last_error = Some(detail.clone());
            TrackEvent {
                source_dir: project.source_dir.clone(),
                application: project.app.clone(),
                version: None,
                kind: TrackEventKind::Error,
                detail,
            }
        }
    }
}

fn try_scan_project(project: &mut TrackedProject, archive: &Archive) -> Result<TrackEvent> {
    let package = read_cargo_package(&project.source_dir)?;
    let application = project.app.clone().unwrap_or(package.name);
    let version = package.version;
    let binary = project.binary.clone().unwrap_or_else(|| {
        project
            .source_dir
            .join("target")
            .join(&project.profile)
            .join(&application)
    });
    let digest = sha256_file(&binary)
        .with_context(|| format!("tracked binary is not ready: {}", binary.display()))?;
    let target = project.target.clone().or_else(host_target);
    let compiler_version = rustc_version();
    let mut source = git::source_info(&project.source_dir);
    if let Some(snapshot) = uni::snapshot_info(&project.source_dir) {
        source.source_tree_hash = Some(snapshot.source_tree_hash);
        if source.commit.is_none() {
            source.commit = snapshot.target_commit;
        }
    }
    let build_context = sha256_bytes(
        format!(
            "commit={}\nsource_tree={}\ndirty={}\ncompiler_version={}\ntarget={}\nprofile={}\nfeatures={}",
            source.commit.as_deref().unwrap_or(""),
            source.source_tree_hash.as_deref().unwrap_or(""),
            source.dirty,
            compiler_version.as_deref().unwrap_or(""),
            target.as_deref().unwrap_or(""),
            project.profile,
            project.features.join(","),
        )
        .as_bytes(),
    );

    let unchanged = project.last_archived.as_ref().is_some_and(|snapshot| {
        snapshot.application == application
            && snapshot.version == version
            && snapshot.sha256 == digest
            && snapshot.build_context == build_context
    });
    if unchanged {
        return Ok(TrackEvent {
            source_dir: project.source_dir.clone(),
            application: Some(application),
            version: Some(version),
            kind: TrackEventKind::Unchanged,
            detail: format!("no change ({digest})"),
        });
    }

    archive.archive(ArchiveRequest {
        application: &application,
        version: &version,
        binary_path: &binary,
        artefact_name: None,
        target,
        profile: Some(project.profile.clone()),
        features: project.features.clone(),
        compiler: Some("rustc".to_string()),
        compiler_version,
        source_dir: &project.source_dir,
    })?;

    project.last_archived = Some(TrackedSnapshot {
        application: application.clone(),
        version: version.clone(),
        sha256: digest.clone(),
        build_context,
        archived_at: Utc::now(),
    });
    Ok(TrackEvent {
        source_dir: project.source_dir.clone(),
        application: Some(application),
        version: Some(version),
        kind: TrackEventKind::Archived,
        detail: format!("archived sha256={digest}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cargo_project(root: &Path, bytes: &[u8]) {
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        let binary_dir = root.join("target/release");
        fs::create_dir_all(&binary_dir).unwrap();
        fs::write(binary_dir.join("demo"), bytes).unwrap();
    }

    #[test]
    fn scan_archives_changes_and_skips_identical_builds() {
        let archive_root = tempdir().unwrap();
        let source = tempdir().unwrap();
        cargo_project(source.path(), b"first");
        let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
        let project =
            TrackedProject::new(source.path(), None, None, None, "release".into(), vec![]).unwrap();
        let mut registry = TrackingRegistry {
            schema: tracking_schema(),
            projects: vec![project],
        };

        let first = registry.scan(&archive);
        assert_eq!(first[0].kind, TrackEventKind::Archived);
        let unchanged = registry.scan(&archive);
        assert_eq!(unchanged[0].kind, TrackEventKind::Unchanged);

        fs::write(source.path().join("target/release/demo"), b"second").unwrap();
        let changed = registry.scan(&archive);
        assert_eq!(changed[0].kind, TrackEventKind::Archived);
        let manifest = archive.inspect("demo", "1.2.3").unwrap();
        assert_eq!(manifest.builds.len(), 1);
        assert_eq!(
            manifest.primary_artefact().unwrap().sha256,
            sha256_file(&source.path().join("target/release/demo")).unwrap()
        );
    }

    #[test]
    fn scan_isolates_project_errors_and_persists_them() {
        let archive_root = tempdir().unwrap();
        let source = tempdir().unwrap();
        fs::write(
            source.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
        let project =
            TrackedProject::new(source.path(), None, None, None, "release".into(), vec![]).unwrap();
        let mut registry = TrackingRegistry {
            schema: tracking_schema(),
            projects: vec![project],
        };

        let events = registry.scan(&archive);
        assert_eq!(events[0].kind, TrackEventKind::Error);
        assert!(registry.projects[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("tracked binary is not ready"));
    }

    #[test]
    fn registry_round_trips_atomically() {
        let dir = tempdir().unwrap();
        cargo_project(dir.path(), b"binary");
        let path = dir.path().join("state/tracking.json");
        let project = TrackedProject::new(
            dir.path(),
            None,
            Some("renamed".into()),
            None,
            "release".into(),
            vec![],
        )
        .unwrap();
        let registry = TrackingRegistry {
            schema: tracking_schema(),
            projects: vec![project],
        };
        registry.save(&path).unwrap();

        let loaded = TrackingRegistry::load(&path).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].app.as_deref(), Some("renamed"));
    }

    #[test]
    fn cargo_workspace_version_is_inherited() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n[workspace.package]\nversion = \"4.5.6\"\n",
        )
        .unwrap();
        let member = root.path().join("member");
        fs::create_dir(&member).unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion.workspace = true\n",
        )
        .unwrap();

        let package = read_cargo_package(&member).unwrap();
        assert_eq!(package.name, "demo");
        assert_eq!(package.version, "4.5.6");
    }

    #[test]
    fn daemon_guard_rejects_live_holder_and_recovers_stale_pid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon.pid");
        fs::write(&path, format!("{}\n", std::process::id())).unwrap();
        let error = DaemonGuard::acquire(path.clone()).err().unwrap();
        assert!(error.to_string().contains("another shallowgrave daemon"));

        fs::write(&path, "4294967295\n").unwrap();
        let guard = DaemonGuard::acquire(path.clone()).unwrap();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }
}
