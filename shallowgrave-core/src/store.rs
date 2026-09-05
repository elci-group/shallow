use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Result, ShallowgraveError};
use crate::hash::sha256_file;

/// Storage boundary from §35 of the directive: keeps GitHub (or any other
/// backend) from becoming an architectural dependency of the core archive logic.
pub trait ArtifactStore {
    fn put(&self, source: &Path) -> Result<String>;
    fn get(&self, digest: &str, dest: &Path) -> Result<()>;
    fn exists(&self, digest: &str) -> bool;
    fn verify(&self, digest: &str) -> Result<bool>;
}

/// Content-addressable local disk store: `objects/<first2>/<digest>`.
/// Identical bytes always land at the same path, giving free deduplication (§27).
pub struct LocalStore {
    objects_dir: PathBuf,
}

impl LocalStore {
    pub fn new(objects_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&objects_dir)?;
        Ok(Self { objects_dir })
    }

    fn path_for(&self, digest: &str) -> PathBuf {
        let prefix = &digest[..2.min(digest.len())];
        self.objects_dir.join(prefix).join(digest)
    }
}

impl ArtifactStore for LocalStore {
    fn put(&self, source: &Path) -> Result<String> {
        if !source.is_file() {
            return Err(ShallowgraveError::ArtefactSourceNotFound(source.to_path_buf()));
        }
        let digest = sha256_file(source)?;
        let dest = self.path_for(&digest);
        if !dest.exists() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, &dest)?;
        }
        Ok(digest)
    }

    fn get(&self, digest: &str, dest: &Path) -> Result<()> {
        let src = self.path_for(digest);
        if !src.exists() {
            return Err(ShallowgraveError::ArtefactNotFound(digest.to_string()));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&src, dest)?;
        Ok(())
    }

    fn exists(&self, digest: &str) -> bool {
        self.path_for(digest).exists()
    }

    fn verify(&self, digest: &str) -> Result<bool> {
        let path = self.path_for(digest);
        if !path.exists() {
            return Err(ShallowgraveError::ArtefactNotFound(digest.to_string()));
        }
        let actual = sha256_file(&path)?;
        Ok(actual == digest)
    }
}
