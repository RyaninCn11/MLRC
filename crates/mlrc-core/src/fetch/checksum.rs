//! Checksums, and what they can and cannot tell you.
//!
//! A checksum detects corruption. It does not establish authenticity: the value comes
//! over the same connection as the file, so TLS is what makes it trustworthy, and the
//! checksum only catches a transfer that went wrong, a mirror that served something
//! else, or a file that was tampered with after it was published.
//!
//! SHA1 is fine for that job - accidental damage is not an adversary - but it is not
//! collision-resistant, so a source that offers something stronger is used at its
//! strongest. Mojang publishes SHA1 and nothing else, Modrinth publishes SHA1 and
//! SHA512, and the authlib-injector API publishes SHA256.

use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    Sha1(String),
    Sha256(String),
    Sha512(String),
}

impl Checksum {
    /// The strongest of the three, or `None` when the source published none.
    pub fn strongest(sha1: &str, sha256: &str, sha512: &str) -> Option<Self> {
        if !sha512.trim().is_empty() {
            Some(Self::Sha512(sha512.trim().to_ascii_lowercase()))
        } else if !sha256.trim().is_empty() {
            Some(Self::Sha256(sha256.trim().to_ascii_lowercase()))
        } else if !sha1.trim().is_empty() {
            Some(Self::Sha1(sha1.trim().to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// The value that was published, for an error message.
    pub fn expected(&self) -> &str {
        match self {
            Self::Sha1(value) | Self::Sha256(value) | Self::Sha512(value) => value,
        }
    }

    pub const fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha1(_) => "sha1",
            Self::Sha256(_) => "sha256",
            Self::Sha512(_) => "sha512",
        }
    }

    pub fn matches(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Sha1(expected) => hex(&Sha1::digest(bytes)) == *expected,
            Self::Sha256(expected) => hex(&Sha256::digest(bytes)) == *expected,
            Self::Sha512(expected) => hex(&Sha512::digest(bytes)) == *expected,
        }
    }

    /// Fails with `E_NET_HASH` when the bytes do not match.
    pub fn verify(&self, bytes: &[u8], subject: &str) -> MlrcResult<()> {
        if self.matches(bytes) {
            return Ok(());
        }

        let actual = match self {
            Self::Sha1(_) => hex(&Sha1::digest(bytes)),
            Self::Sha256(_) => hex(&Sha256::digest(bytes)),
            Self::Sha512(_) => hex(&Sha512::digest(bytes)),
        };

        Err(MlrcError::new(
            ErrorCode::HashMismatch,
            format!(
                "{} mismatch for {subject}: expected {}, got {actual}",
                self.algorithm(),
                self.expected()
            ),
        )
        .with("algorithm", self.algorithm())
        .with("expected", self.expected().to_string())
        .with("actual", actual)
        .with("subject", subject.to_string()))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha1("") and sha256("") and sha512("") are the standard empty-string digests.
    const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const EMPTY_SHA512: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
                                47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

    #[test]
    fn the_strongest_published_hash_wins() {
        assert_eq!(
            Checksum::strongest(EMPTY_SHA1, "", EMPTY_SHA512),
            Some(Checksum::Sha512(EMPTY_SHA512.to_string()))
        );
        assert_eq!(
            Checksum::strongest(EMPTY_SHA1, EMPTY_SHA256, ""),
            Some(Checksum::Sha256(EMPTY_SHA256.to_string()))
        );
        assert_eq!(
            Checksum::strongest(EMPTY_SHA1, "", ""),
            Some(Checksum::Sha1(EMPTY_SHA1.to_string()))
        );
        assert_eq!(Checksum::strongest("", "", ""), None);
    }

    #[test]
    fn verification_accepts_and_rejects() {
        let sha512 = Checksum::strongest("", "", EMPTY_SHA512).expect("checksum");
        assert!(sha512.verify(b"", "empty").is_ok());

        let error = sha512.verify(b"not empty", "empty").expect_err("mismatch");
        assert_eq!(error.code, ErrorCode::HashMismatch);
        assert_eq!(
            error.context.get("algorithm"),
            Some(&serde_json::json!("sha512"))
        );
    }

    #[test]
    fn values_are_compared_case_insensitively() {
        let checksum =
            Checksum::strongest(&EMPTY_SHA1.to_ascii_uppercase(), "", "").expect("uppercase sha1");
        assert!(checksum.matches(b""));
    }
}
