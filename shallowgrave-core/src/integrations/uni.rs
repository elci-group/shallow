use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::hash::sha256_bytes;

/// Best-effort UNI integration (§14): consumes UNI's already-written source
/// snapshot rather than invoking `uni` ourselves — a full `uni --json` run
/// drives several analyzer tools and is far too slow to run on every archive.
/// Absence of the snapshot is not an error; archival proceeds without it.
#[derive(Debug, Clone)]
pub struct UniSnapshotInfo {
    /// Hash over the recorded source tree (relative_path + per-file sha256),
    /// filling the `source_tree` component of the build fingerprint (§5).
    pub source_tree_hash: String,
    /// UNI's own recorded commit, usable as a fallback when `git` is
    /// unavailable or the directory predates any commit.
    pub target_commit: Option<String>,
}

#[derive(Deserialize)]
struct Snapshot {
    target_commit: Option<String>,
    files: Vec<SnapshotFile>,
}

#[derive(Deserialize)]
struct SnapshotFile {
    relative_path: String,
    sha256: String,
}

pub fn snapshot_info(source_dir: &Path) -> Option<UniSnapshotInfo> {
    let path = source_dir.join(".uni").join("snapshot-v1.json");
    let text = fs::read_to_string(path).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&text).ok()?;

    let mut entries: Vec<String> = snapshot
        .files
        .iter()
        .map(|f| format!("{}:{}", f.relative_path, f.sha256))
        .collect();
    entries.sort();
    let source_tree_hash = sha256_bytes(entries.join("\n").as_bytes());

    Some(UniSnapshotInfo { source_tree_hash, target_commit: snapshot.target_commit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_snapshot_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(snapshot_info(dir.path()).is_none());
    }

    #[test]
    fn hash_is_order_independent_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".uni")).unwrap();
        let snapshot = r#"{
            "target_commit": "abc123",
            "files": [
                {"relative_path": "b.rs", "sha256": "222"},
                {"relative_path": "a.rs", "sha256": "111"}
            ]
        }"#;
        fs::write(dir.path().join(".uni/snapshot-v1.json"), snapshot).unwrap();
        let info = snapshot_info(dir.path()).unwrap();
        assert_eq!(info.target_commit.as_deref(), Some("abc123"));

        // Same files, different order in the source file -> same hash.
        let reordered = r#"{
            "target_commit": "abc123",
            "files": [
                {"relative_path": "a.rs", "sha256": "111"},
                {"relative_path": "b.rs", "sha256": "222"}
            ]
        }"#;
        fs::write(dir.path().join(".uni/snapshot-v1.json"), reordered).unwrap();
        let info2 = snapshot_info(dir.path()).unwrap();
        assert_eq!(info.source_tree_hash, info2.source_tree_hash);
    }
}
