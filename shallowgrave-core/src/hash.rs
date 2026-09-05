use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Streaming SHA-256 so multi-gigabyte binaries never load fully into memory.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_same_digest() {
        // Core invariant from §39: same bytes -> same artefact ID.
        assert_eq!(sha256_bytes(b"hello"), sha256_bytes(b"hello"));
        assert_ne!(sha256_bytes(b"hello"), sha256_bytes(b"world"));
    }

    #[test]
    fn known_vector() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn file_hash_matches_byte_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"content").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(b"content"));
    }
}
