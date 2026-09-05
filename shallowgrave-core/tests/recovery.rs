use std::fs;

use shallowgrave_core::manifest::LifecycleState;
use shallowgrave_core::{Archive, ArchiveRequest};
use tempfile::tempdir;

fn request<'a>(
    app: &'a str,
    version: &'a str,
    binary_path: &'a std::path::Path,
    target: Option<&str>,
    source_dir: &'a std::path::Path,
) -> ArchiveRequest<'a> {
    ArchiveRequest {
        application: app,
        version,
        binary_path,
        artefact_name: None,
        target: target.map(str::to_string),
        profile: Some("release".to_string()),
        features: vec![],
        compiler: Some("rustc".to_string()),
        compiler_version: None,
        source_dir,
    }
}

#[test]
fn archive_delete_resurrect_verify_roundtrip() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();

    let binary_path = work_dir.path().join("myapp");
    fs::write(&binary_path, b"pretend this is a compiled binary").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    let manifest = archive
        .archive(request("myapp", "1.0.0", &binary_path, Some("x86_64-unknown-linux-gnu"), work_dir.path()))
        .unwrap();
    let expected_sha256 = manifest.primary_artefact().unwrap().sha256.clone();

    // Simulate the acceptance-criteria scenario (§49): delete every local build artefact.
    fs::remove_file(&binary_path).unwrap();
    assert!(!binary_path.exists());

    let versions = archive.find("myapp").unwrap();
    assert_eq!(versions, vec!["1.0.0".to_string()]);

    let restored_path = work_dir.path().join("restored-myapp");
    let restored_manifest = archive.resurrect("myapp", "1.0.0", None, &restored_path).unwrap();
    assert_eq!(restored_manifest.primary_artefact().unwrap().sha256, expected_sha256);
    assert_eq!(fs::read(&restored_path).unwrap(), b"pretend this is a compiled binary");

    let verification = archive.verify("myapp", "1.0.0", None).unwrap();
    assert!(verification.ok);
    assert_eq!(verification.expected_sha256, expected_sha256);
}

#[test]
fn multi_platform_builds_stay_separately_identified() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();

    let linux_binary = work_dir.path().join("linux-bin");
    let mac_binary = work_dir.path().join("mac-bin");
    fs::write(&linux_binary, b"linux bytes").unwrap();
    fs::write(&mac_binary, b"mac bytes").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    archive
        .archive(request("uni", "1.4.2", &linux_binary, Some("x86_64-unknown-linux-gnu"), work_dir.path()))
        .unwrap();
    let manifest = archive
        .archive(request("uni", "1.4.2", &mac_binary, Some("aarch64-apple-darwin"), work_dir.path()))
        .unwrap();

    assert_eq!(manifest.builds.len(), 2);
    let linux_artefact = manifest.artefact_for_target("x86_64-unknown-linux-gnu").unwrap();
    let mac_artefact = manifest.artefact_for_target("aarch64-apple-darwin").unwrap();
    assert_ne!(linux_artefact.sha256, mac_artefact.sha256);

    let linux_verify = archive.verify("uni", "1.4.2", Some("x86_64-unknown-linux-gnu")).unwrap();
    let mac_verify = archive.verify("uni", "1.4.2", Some("aarch64-apple-darwin")).unwrap();
    assert!(linux_verify.ok && mac_verify.ok);
}

#[test]
fn rearchiving_same_build_replaces_its_artefact_not_the_other_builds() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();
    let binary_path = work_dir.path().join("bin");
    fs::write(&binary_path, b"v1").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    let first = archive
        .archive(request("app", "1.0.0", &binary_path, Some("x86_64-unknown-linux-gnu"), work_dir.path()))
        .unwrap();
    let first_sha256 = first.primary_artefact().unwrap().sha256.clone();

    // Same target/profile/features/commit -> same fingerprint -> idempotent update, not a new build.
    fs::write(&binary_path, b"v1-rebuilt-same-fingerprint").unwrap();
    let manifest = archive
        .archive(request("app", "1.0.0", &binary_path, Some("x86_64-unknown-linux-gnu"), work_dir.path()))
        .unwrap();
    assert_eq!(manifest.builds.len(), 1);
    let second_sha256 = manifest.primary_artefact().unwrap().sha256.clone();
    assert_ne!(first_sha256, second_sha256, "the artefact under that build must reflect the rebuilt bytes");
}

#[test]
fn deprecate_and_retire_never_delete_artefacts() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();
    let binary_path = work_dir.path().join("bin");
    fs::write(&binary_path, b"bytes").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    archive.archive(request("app", "1.0.0", &binary_path, None, work_dir.path())).unwrap();

    let deprecated = archive.deprecate("app", "1.0.0").unwrap();
    assert_eq!(deprecated.manifest.status, LifecycleState::Deprecated);
    assert!(deprecated.manifest.lifecycle.deprecated.is_some());
    assert!(!deprecated.backward_warning);

    let retired = archive.retire("app", "1.0.0").unwrap();
    assert_eq!(retired.manifest.status, LifecycleState::Retired);

    // The artefact must still resurrect after lifecycle changes.
    let verification = archive.verify("app", "1.0.0", None).unwrap();
    assert!(verification.ok);
}

#[test]
fn doctor_reports_missing_artefact_and_gc_leaves_referenced_objects() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();
    let referenced = work_dir.path().join("referenced");
    let orphan_source = work_dir.path().join("orphan");
    fs::write(&referenced, b"kept").unwrap();
    fs::write(&orphan_source, b"orphaned bytes").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    archive.archive(request("app", "1.0.0", &referenced, None, work_dir.path())).unwrap();

    // Put an object in the store with no manifest pointing to it.
    let orphan_digest = shallowgrave_core::hash::sha256_file(&orphan_source).unwrap();
    let orphan_dir = archive_root.path().join("objects").join(&orphan_digest[..2]);
    fs::create_dir_all(&orphan_dir).unwrap();
    fs::copy(&orphan_source, orphan_dir.join(&orphan_digest)).unwrap();

    let report_before = archive.doctor().unwrap();
    assert!(report_before.is_healthy());
    assert_eq!(report_before.orphaned_objects, vec![orphan_digest.clone()]);

    let gc_report = archive.gc_orphaned().unwrap();
    assert_eq!(gc_report.removed_objects, vec![orphan_digest]);

    // The referenced release must still verify after gc.
    let verification = archive.verify("app", "1.0.0", None).unwrap();
    assert!(verification.ok);

    let report_after = archive.doctor().unwrap();
    assert!(report_after.orphaned_objects.is_empty());
}

#[test]
fn verify_reports_failure_when_object_missing() {
    let archive_root = tempdir().unwrap();
    let work_dir = tempdir().unwrap();
    let binary_path = work_dir.path().join("myapp");
    fs::write(&binary_path, b"bytes").unwrap();

    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    archive.archive(request("myapp", "1.0.0", &binary_path, None, work_dir.path())).unwrap();

    // Corrupt the archive by wiping the object store, independent of the manifest.
    fs::remove_dir_all(archive_root.path().join("objects")).unwrap();

    let verification = archive.verify("myapp", "1.0.0", None).unwrap();
    assert!(!verification.ok);
    assert!(verification.actual_sha256.is_none());

    let report = archive.doctor().unwrap();
    assert!(!report.is_healthy());
    assert_eq!(report.missing_artefacts.len(), 1);
}

#[test]
fn inspect_unknown_release_errors() {
    let archive_root = tempdir().unwrap();
    let archive = Archive::open(archive_root.path().to_path_buf()).unwrap();
    let err = archive.inspect("nope", "1.0.0").unwrap_err();
    assert!(matches!(err, shallowgrave_core::ShallowgraveError::ReleaseNotFound(_, _)));
}
