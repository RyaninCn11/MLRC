//! Offline identity.
//!
//! The UUID is the well-known offline derivation: MD5 of `OfflinePlayer:<name>`,
//! with the RFC 4122 version-3 and variant bits forced. Servers running
//! `online-mode=false` recognise it, which is what makes LAN play work.

use md5::{Digest, Md5};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

#[derive(Debug, Clone)]
pub struct Identity {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: String,
    pub xuid: String,
    pub client_id: String,
}

/// Mojang's own rule: 1-16 characters, letters, digits and underscore.
///
/// Offline servers tolerate more, but a name that would be rejected by an
/// online-mode server is a trap worth refusing up front, and the game's own UI uses
/// the same rule.
pub fn validate_name(name: &str) -> MlrcResult<()> {
    let valid = !name.is_empty()
        && name.chars().count() <= 16
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');

    if valid {
        Ok(())
    } else {
        Err(MlrcError::new(
            ErrorCode::Usage,
            format!("invalid player name {name}: 1-16 characters, letters, digits and '_'"),
        )
        .with("name", name.to_string()))
    }
}

pub fn offline(name: &str) -> Identity {
    let name = name.trim();
    let name = if name.is_empty() { "Player" } else { name };

    Identity {
        name: name.to_string(),
        uuid: offline_uuid(name),
        // The game only echoes this back to the server; offline servers ignore it.
        access_token: "0".to_string(),
        user_type: "legacy".to_string(),
        xuid: String::new(),
        client_id: String::new(),
    }
}

pub fn offline_uuid(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{name}").as_bytes());

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hasher.finalize());
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The game wants the UUID without dashes in `--uuid`.
pub fn uuid_without_dashes(uuid: &str) -> String {
    uuid.replace('-', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notch_vector_matches_the_offline_derivation() {
        assert_eq!(
            offline_uuid("Notch"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
    }

    #[test]
    fn empty_names_fall_back_to_player() {
        assert_eq!(offline("  ").name, "Player");
        assert_eq!(offline("").name, "Player");
    }

    #[test]
    fn uuid_is_trimmed_for_the_command_line() {
        assert_eq!(
            uuid_without_dashes("b50ad385-829d-3141-a216-7e7d7539ba7f"),
            "b50ad385829d3141a2167e7d7539ba7f"
        );
    }
}
