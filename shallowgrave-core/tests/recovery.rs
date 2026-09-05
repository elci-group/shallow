use std::fs;

use shallowgrave_core::{Archive, ArchiveRequest};
use tempfile::tempdir;

#[test]
fn archive_delete_resurrect_verify_roundtrip() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();

    let binary_path = work_dir.path().join("myapp");
    fs::write(&binary_path, b"pretend this is a compiled binary").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    let manifest = archive
        .archive(ArchiveRequest {
            application: "myapp",
            version: "1.0.0",
            binary_path: &binary_path,
            artefact_name: None,
            target: Some("x86_64-unknown-linux-gnu".to_string()),
            profile: Some("release".to_string()),
            features: vec![],
            compiler: Some("rustc".to_string()),
            compiler_version: None,
            source_dir: work_dir.path(),
        })
        .unwrap();
    let expected_sha256 = manifest.primary_artefact().unwrap().sha256.clone();

    // Simulate the acceptance-criteria scenario (§49): delete every local build artefact.
    fs::remove_file(&binary_path).unwrap();
    assert!(!binary_path.exists());

    let versions = archive.find("myapp").unwrap();
    assert_eq!(versions, vec!["1.0.0".to_string()]);

    let restored_path = work_dir.path().join("restored-myapp");
    let restored_manifest = archive.resurrect("myapp", "1.0.0", &restored_path).unwrap();
    assert_eq!(restored_manifest.primary_artefact().unwrap().sha256, expected_sha256);
    assert_eq!(fs::read(&restored_path).unwrap(), b"pretend this is a compiled binary");

    let verification = archive.verify("myapp", "1.0.0").unwrap();
    assert!(verification.ok);
    assert_eq!(verification.expected_sha256, expected_sha256);
}

#[test]
fn verify_reports_failure_when_object_missing() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();
    let binary_path = work_dir.path().join("myapp");
    fs::write(&binary_path, b"bytes").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    archive
        .archive(ArchiveRequest {
            application: "myapp",
            version: "1.0.0",
            binary_path: &binary_path,
            artefact_name: None,
            target: None,
            profile: None,
            features: vec![],
            compiler: None,
            compiler_version: None,
            source_dir: work_dir.path(),
        })
        .unwrap();

    // Corrupt the archive by wiping the object store, independent of the manifest.
    fs::remove_dir_all(archive_root.path().join("objects")).unwrap();

    let verification = archive.verify("myapp", "1.0.0").unwrap();
    assert!(!verification.ok);
    assert!(verification.actual_sha256.is_none());
}

#[test]
fn inspect_unknown_release_errors() {
    let archive_root = tempdir().unwrap();
    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    let err = archive.inspect("nope", "1.0.0").unwrap_err();
    assert!(matches!(err, shallowgrave_core::ShallowgraveError::ReleaseNotFound(_, _)));
}
