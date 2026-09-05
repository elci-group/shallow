use std::path::Path;
use std::process::Command;

use crate::manifest::SourceInfo;

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Best-effort: returns default (all-None, dirty=false) when `dir` is not inside a
/// git repository or the `git` binary is unavailable, rather than failing archival.
pub fn source_info(dir: &Path) -> SourceInfo {
    let commit = run_git(dir, &["rev-parse", "HEAD"]);
    let repository = run_git(dir, &["remote", "get-url", "origin"]);
    let dirty = run_git(dir, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    SourceInfo { repository, commit, dirty, source_tree_hash: None }
}
