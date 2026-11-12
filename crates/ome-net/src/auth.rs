use sha2::{Digest, Sha256};

pub type SecretHash = [u8; 32];

pub fn hash_secret(username: &str, secret: &str) -> SecretHash {
    let mut hasher = Sha256::new();
    hasher.update(b"ome:");
    hasher.update(username.as_bytes());
    hasher.update(b":");
    hasher.update(secret.as_bytes());
    hasher.finalize().into()
}

pub fn verify(expected: &SecretHash, username: &str, secret: &str) -> bool {
    let actual = hash_secret(username, secret);
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

pub fn valid_username(username: &str) -> bool {
    (1..=32).contains(&username.len())
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_only_the_right_secret_for_the_right_user() {
        let hash = hash_secret("alice", "pw");
        assert!(verify(&hash, "alice", "pw"));
        assert!(!verify(&hash, "alice", "pw2"));
        assert!(!verify(&hash, "bob", "pw"));
    }

    #[test]
    fn validates_usernames() {
        assert!(valid_username("mm_acme-1"));
        assert!(!valid_username(""));
        assert!(!valid_username("has space"));
        assert!(!valid_username(&"x".repeat(33)));
    }
}
