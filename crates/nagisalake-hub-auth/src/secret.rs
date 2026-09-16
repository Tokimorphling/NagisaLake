//! Opaque bearer secret generation and hashing.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSecret {
    pub plaintext:      String,
    pub display_prefix: String,
    pub hash:           String,
}

/// Generates a high-entropy opaque token. The full value is returned once.
pub fn generate_secret(prefix: &str) -> GeneratedSecret {
    let random = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let plaintext = format!("{prefix}_{random}");
    let display_prefix = plaintext.chars().take(prefix.len() + 9).collect();
    let hash = hash_secret(&plaintext);
    GeneratedSecret {
        plaintext,
        display_prefix,
        hash,
    }
}

/// SHA-256 is appropriate for uniformly random 244-bit bearer secrets.
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hash
}

pub fn verify_secret(secret: &str, expected_hash: &str) -> bool {
    constant_time_eq(hash_secret(secret).as_bytes(), expected_hash.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut different = 0_u8;
    for (left, right) in left.iter().zip(right) {
        different |= left ^ right;
    }
    different == 0
}
