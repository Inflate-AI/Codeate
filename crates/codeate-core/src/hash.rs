use sha2::{Digest, Sha256};

/// Compute a stable, deterministic hex digest of `bytes`.
///
/// Used as the content identity for ingested files: same bytes always
/// produce the same digest, regardless of platform or invocation.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_input_produces_the_same_hash() {
        assert_eq!(hash_bytes(b"hello codeate"), hash_bytes(b"hello codeate"));
    }

    #[test]
    fn different_input_produces_a_different_hash() {
        assert_ne!(hash_bytes(b"hello codeate"), hash_bytes(b"goodbye codeate"));
    }

    #[test]
    fn empty_input_has_a_defined_hash() {
        // sha256("") is a well-known constant digest.
        assert_eq!(
            hash_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn digest_is_a_fixed_length_lowercase_hex_string() {
        let digest = hash_bytes(b"some file content");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
