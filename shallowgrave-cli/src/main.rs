use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
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
        /// Path to the built binary to archive.
        binary: PathBuf,
        #[arg(long)]
        app: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "feature", value_name = "FEATURE")]
        features: Vec<String>,
        /// Directory to read git provenance from. Defaults to the current directory.
        #[arg(long)]
        source_dir: Option<PathBuf>,
    },
    /// List known versions of an application.
    Find { app: String },
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
    },
    /// Restore a historical artefact locally.
    Resurrect {
        /// `app@version`
        release: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.archive.unwrap_or_else(default_archive_root);
    let archive = Archive::open(root)?;

    match cli.command {
        Command::Archive { binary, app, version, target, profile, features, source_dir } => {
            let source_dir = source_dir.unwrap_or_else(|| PathBuf::from("."));
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
            let artefact = manifest.primary_artefact().expect("archive always writes one artefact");
            println!("✓ archived {}@{}", manifest.application, manifest.version);
            println!("  artefact: {}", artefact.name);
            println!("  sha256:   {}", artefact.sha256);
            if let Some(commit) = &manifest.source.commit {
                println!("  source:   {commit}");
                if manifest.source.dirty {
                    println!("  warning:  working tree was dirty at archive time (NON_REPRODUCIBLE_SOURCE_STATE)");
                }
            }
        }
        Command::Find { app } => {
            let versions = archive.find(&app)?;
            for v in versions {
                println!("{v}");
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
                println!("  target:   {}", manifest.build.target.as_deref().unwrap_or("unknown"));
                println!("  fingerprint: {}", manifest.build.fingerprint);
                for a in &manifest.artefacts {
                    println!("  artefact: {} ({}) sha256={} size={}", a.name, a.artefact_type, a.sha256, a.size);
                }
            }
        }
        Command::Verify { release } => {
            let (app, version) = split_release(&release)?;
            let result = archive.verify(app, version)?;
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
        Command::Resurrect { release, output } => {
            let (app, version) = split_release(&release)?;
            let dest = output.unwrap_or_else(|| PathBuf::from(app));
            let manifest = archive.resurrect(app, version, &dest)?;
            let artefact = manifest.primary_artefact().expect("resurrected release always has one artefact");
            println!("Shallowgrave");
            println!();
            println!("Application: {}", manifest.application);
            println!("Version:     {}", manifest.version);
            if let Some(target) = &manifest.build.target {
                println!("Target:      {target}");
            }
            println!();
            println!("Artefact verified.");
            println!("SHA-256: {}", artefact.sha256);
            println!();
            println!("Restored to: {}", resolve_display_path(&dest));
            println!("Not executed automatically — run it explicitly when ready.");
        }
    }

    Ok(())
}

fn resolve_display_path(p: &Path) -> String {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf()).display().to_string()
}

fn rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}
