use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use shallowgrave_core::github::{self, GhRemote};
use shallowgrave_core::{Archive, ArchiveRequest};

#[derive(Parser)]
#[command(name = "shallowgrave", version, about = "Immutable historical archive of software releases and their executable artefacts")]
struct Cli {
    /// Archive root directory. Defaults to $SHALLOWGRAVE_HOME or ~/.shallowgrave.
    #[arg(long, global = true)]
    archive: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Archive a built binary as a release.
    Archive {
        /// Path to the built binary to archive. Optional with --auto (defaults to
        /// target/<profile>/<name> under --source-dir).
        binary: Option<PathBuf>,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "feature", value_name = "FEATURE")]
        features: Vec<String>,
        /// Directory to read git/UNI/Cargo.toml provenance from. Defaults to the current directory.
        #[arg(long)]
        source_dir: Option<PathBuf>,
        /// Infer app/version from Cargo.toml, target from the host toolchain,
        /// and the binary path from target/<profile>/<app> — the shape a
        /// release orchestrator like Kaptaind would invoke (§13/§15).
        #[arg(long)]
        auto: bool,
        #[arg(long)]
        json: bool,
    },
    /// List known versions of an application.
    Find {
        app: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the manifest for a specific release.
    Inspect {
        /// `app@version`
        release: String,
        #[arg(long)]
        json: bool,
    },
    /// Verify artefact integrity for a specific release.
    Verify {
        /// `app@version`
        release: String,
        /// Platform to verify when the release has multiple builds. Defaults to the first build.
        #[arg(long)]
        target: Option<String>,
    },
    /// Restore a historical artefact locally.
    Resurrect {
        /// `app@version`
        release: String,
        /// Platform to restore when the release has multiple builds. Defaults to the first build.
        #[arg(long)]
        target: Option<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Mark a release deprecated. Never deletes anything (§10).
    Deprecate { release: String },
    /// Mark a release retired. Never deletes anything (§10).
    Retire { release: String },
    /// Audit local archive integrity.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Remove objects no manifest references. Never touches an object a manifest still needs (§25).
    Gc {
        #[arg(long)]
        dry_run: bool,
    },
    /// Publish archived artefacts to a GitHub Releases backend (§8, §31).
    Sync {
        /// `app@version`. If omitted, syncs every archived release.
        release: Option<String>,
        /// `owner/name` of the GitHub repository to publish to.
        #[arg(long)]
        repo: String,
        /// Actually create releases and upload assets. Without this, only prints the plan —
        /// publishing releases is a visible, external action and shouldn't happen by accident.
        #[arg(long)]
        execute: bool,
    },
}

fn default_archive_root() -> PathBuf {
    if let Ok(p) = std::env::var("SHALLOWGRAVE_HOME") {
        return PathBuf::from(p);
    }
    dirs_home().join(".shallowgrave")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}

fn split_release(release: &str) -> Result<(&str, &str)> {
    release
        .split_once('@')
        .ok_or_else(|| anyhow!("expected `app@version`, got `{release}`"))
}

#[derive(Deserialize)]
struct CargoToml {
    package: CargoPackage,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
}

fn read_cargo_package(source_dir: &Path) -> Result<CargoPackage> {
    let text = std::fs::read_to_string(source_dir.join("Cargo.toml"))
        .with_context(|| format!("--auto needs a Cargo.toml under {}", source_dir.display()))?;
    Ok(toml::from_str::<CargoToml>(&text)?.package)
}

fn host_target() -> Option<String> {
    let output = std::process::Command::new("rustc").args(["-vV"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.lines().find_map(|line| line.strip_prefix("host: ").map(str::to_string))
}

fn rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

fn resolve_display_path(p: &Path) -> String {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf()).display().to_string()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.archive.unwrap_or_else(default_archive_root);
    let archive = Archive::open(root)?;

    match cli.command {
        Command::Archive { binary, app, version, target, profile, features, source_dir, auto, json } => {
            let source_dir = source_dir.unwrap_or_else(|| PathBuf::from("."));

            let (app, version, target, profile, binary) = if auto {
                let package = read_cargo_package(&source_dir)?;
                let app = app.unwrap_or(package.name);
                let version = version.unwrap_or(package.version);
                let target = target.or_else(host_target);
                let profile = profile.or_else(|| Some("release".to_string()));
                let binary = binary.unwrap_or_else(|| {
                    source_dir.join("target").join(profile.as_deref().unwrap_or("release")).join(&app)
                });
                (app, version, target, profile, binary)
            } else {
                let app = app.ok_or_else(|| anyhow!("--app is required without --auto"))?;
                let version = version.ok_or_else(|| anyhow!("--version is required without --auto"))?;
                let binary = binary.ok_or_else(|| anyhow!("BINARY is required without --auto"))?;
                (app, version, target, profile, binary)
            };

            let manifest = archive.archive(ArchiveRequest {
                application: &app,
                version: &version,
                binary_path: &binary,
                artefact_name: None,
                target,
                profile,
                features,
                compiler: Some("rustc".to_string()),
                compiler_version: rustc_version(),
                source_dir: &source_dir,
            })?;

            if json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
                return Ok(());
            }

            let latest_build = manifest.builds.last().expect("archive always appends or updates a build");
            let artefact = latest_build.artefacts.first().expect("archive always writes one artefact");
            println!("✓ archived {}@{}", manifest.application, manifest.version);
            println!("  artefact: {}", artefact.name);
            println!("  target:   {}", latest_build.build.target.as_deref().unwrap_or("unknown"));
            println!("  sha256:   {}", artefact.sha256);
            if let Some(commit) = &manifest.source.commit {
                println!("  source:   {commit}");
            }
            if manifest.source.dirty {
                println!("  warning:  working tree was dirty at archive time (NON_REPRODUCIBLE_SOURCE_STATE)");
            }
        }
        Command::Find { app, json } => {
            let versions = archive.find(&app)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&versions)?);
            } else {
                for v in versions {
                    println!("{v}");
                }
            }
        }
        Command::Inspect { release, json } => {
            let (app, version) = split_release(&release)?;
            let manifest = archive.inspect(app, version)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!("{} {}", manifest.application, manifest.version);
                println!("  status:   {:?}", manifest.status);
                println!("  commit:   {}", manifest.source.commit.as_deref().unwrap_or("unknown"));
                for build in &manifest.builds {
                    println!("  build target={}", build.build.target.as_deref().unwrap_or("unknown"));
                    println!("    fingerprint: {}", build.build.fingerprint);
                    for a in &build.artefacts {
                        println!("    artefact: {} ({}) sha256={} size={}", a.name, a.artefact_type, a.sha256, a.size);
                    }
                }
            }
        }
        Command::Verify { release, target } => {
            let (app, version) = split_release(&release)?;
            let result = archive.verify(app, version, target.as_deref())?;
            if result.ok {
                println!("✓ {}@{} verified", result.application, result.version);
                println!("  sha256: {}", result.expected_sha256);
            } else {
                println!("✗ {}@{} FAILED verification", result.application, result.version);
                println!("  expected: {}", result.expected_sha256);
                match &result.actual_sha256 {
                    Some(a) => println!("  actual:   {a}"),
                    None => println!("  actual:   artefact missing"),
                }
                std::process::exit(1);
            }
        }
        Command::Resurrect { release, target, output } => {
            let (app, version) = split_release(&release)?;
            let dest = output.unwrap_or_else(|| PathBuf::from(app));
            let manifest = archive.resurrect(app, version, target.as_deref(), &dest)?;
            let artefact = target
                .as_deref()
                .and_then(|t| manifest.artefact_for_target(t))
                .or_else(|| manifest.primary_artefact())
                .expect("resurrected release always has one artefact");
            println!("Shallowgrave");
            println!();
            println!("Application: {}", manifest.application);
            println!("Version:     {}", manifest.version);
            if let Some(t) = &target {
                println!("Target:      {t}");
            }
            println!();
            println!("Artefact verified.");
            println!("SHA-256: {}", artefact.sha256);
            println!();
            println!("Restored to: {}", resolve_display_path(&dest));
            println!("Not executed automatically — run it explicitly when ready.");
        }
        Command::Deprecate { release } => {
            let (app, version) = split_release(&release)?;
            let transition = archive.deprecate(app, version)?;
            println!("✓ {}@{} marked deprecated", transition.manifest.application, transition.manifest.version);
            if transition.backward_warning {
                println!("  warning: this moves the release backward in the lifecycle ordering (§10)");
            }
        }
        Command::Retire { release } => {
            let (app, version) = split_release(&release)?;
            let transition = archive.retire(app, version)?;
            println!("✓ {}@{} marked retired", transition.manifest.application, transition.manifest.version);
            if transition.backward_warning {
                println!("  warning: this moves the release backward in the lifecycle ordering (§10)");
            }
        }
        Command::Doctor { json } => {
            let report = archive.doctor()?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "applications": report.applications,
                        "releases": report.releases,
                        "artefact_entries": report.artefact_entries,
                        "unique_objects": report.unique_objects,
                        "orphaned_objects": report.orphaned_objects.len(),
                        "corrupt_manifests": report.corrupt_manifests.len(),
                        "missing_artefacts": report.missing_artefacts.len(),
                        "digest_mismatches": report.digest_mismatches.len(),
                        "inconsistent_lifecycle": report.inconsistent_lifecycle.len(),
                        "health_score": report.health_score(),
                    })
                );
            } else {
                println!("Shallowgrave Doctor");
                println!();
                println!("Applications:       {}", report.applications);
                println!("Releases:           {}", report.releases);
                println!("Artefact entries:   {}", report.artefact_entries);
                println!("Unique objects:     {}", report.unique_objects);
                println!();
                println!("Integrity:");
                println!("  ✓ {} artefacts checked", report.artefact_entries);
                println!("  ! {} missing", report.missing_artefacts.len());
                println!("  ✗ {} corrupt (digest mismatch)", report.digest_mismatches.len());
                println!();
                println!("Manifests:");
                println!("  ✗ {} corrupt", report.corrupt_manifests.len());
                println!("  ! {} inconsistent lifecycle", report.inconsistent_lifecycle.len());
                println!();
                println!("Orphaned objects (gc candidates): {}", report.orphaned_objects.len());
                println!();
                println!("Archive health: {}/100", report.health_score());
            }
            if !report.is_healthy() {
                std::process::exit(1);
            }
        }
        Command::Gc { dry_run } => {
            if dry_run {
                let report = archive.doctor()?;
                println!("{} orphaned objects would be removed", report.orphaned_objects.len());
                for digest in report.orphaned_objects {
                    println!("  {digest}");
                }
            } else {
                let report = archive.gc_orphaned()?;
                println!("✓ removed {} orphaned objects ({} bytes freed)", report.removed_objects.len(), report.freed_bytes);
            }
        }
        Command::Sync { release, repo, execute } => {
            let releases: Vec<(String, String)> = match release {
                Some(r) => {
                    let (app, version) = split_release(&r)?;
                    vec![(app.to_string(), version.to_string())]
                }
                None => {
                    let mut all = Vec::new();
                    for app in archive.applications()? {
                        for version in archive.find(&app)? {
                            all.push((app.clone(), version));
                        }
                    }
                    all
                }
            };

            let remote = GhRemote::new(repo.clone());
            for (app, version) in releases {
                let manifest = archive.inspect(&app, &version)?;
                let tag = github::release_tag(&app, &version);
                let existing = remote.list_release_assets(&tag).unwrap_or(None).unwrap_or_default();
                let planned = github::plan_uploads(&manifest, &existing);

                if planned.is_empty() {
                    println!("= {app}@{version} already fully synced to {repo} ({tag})");
                    continue;
                }

                println!("{} {app}@{version} -> {repo} ({tag}):", if execute { "↑" } else { "would upload" });
                for item in &planned {
                    println!("    {} (target={})", item.label, item.build_target.as_deref().unwrap_or("unknown"));
                }

                if execute {
                    remote.ensure_release(&tag, &format!("{app} {version}"))?;
                    let staging = std::env::temp_dir().join("shallowgrave-sync-export");
                    std::fs::create_dir_all(&staging)?;
                    for build in &manifest.builds {
                        for artefact in &build.artefacts {
                            let label = github::asset_label(&artefact.name, build.build.target.as_deref());
                            if planned.iter().any(|p| p.label == label) {
                                let staged_path = staging.join(&artefact.sha256);
                                archive.export_object(&artefact.sha256, &staged_path)?;
                                remote.upload_asset(&tag, &staged_path, &label)?;
                                let _ = std::fs::remove_file(&staged_path);
                            }
                        }
                    }
                    println!("  ✓ uploaded");
                }
            }
            if !execute {
                println!();
                println!("Dry run only — pass --execute to actually publish to {repo}.");
            }
        }
    }

    Ok(())
}

