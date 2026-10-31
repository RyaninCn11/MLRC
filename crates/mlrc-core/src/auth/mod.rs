//! Identities and how they are obtained.
//!
//! Three sources, in the order they win:
//!
//! 1. a Yggdrasil account stored by `mlrc auth login`, when a server is configured;
//! 2. the player name from the config, in offline mode (the default);
//! 3. `--username`, which overrides both.

pub mod nide8;
pub mod offline;
pub mod yggdrasil;

pub use offline::{offline, uuid_without_dashes, Identity};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Validates a Yggdrasil API root.
///
/// The value is an origin plus path, no trailing slash: `https://littleskin.cn/api/yggdrasil`.
/// Every request is relative to it, so a typo here fails every login request with a
/// 404 that says nothing useful - better to reject it at `config set`.
pub fn validate_server(value: &str) -> MlrcResult<String> {
    let text = value.trim();
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"));

    let Some(rest) = rest else {
        return Err(MlrcError::new(
            ErrorCode::Config,
            format!("invalid auth server {text}: expected an http(s) url"),
        )
        .with("server", text.to_string()));
    };

    if rest.is_empty() || rest.starts_with('/') || rest.contains(' ') {
        return Err(MlrcError::new(
            ErrorCode::Config,
            format!("invalid auth server {text}: expected http(s)://host[/path]"),
        )
        .with("server", text.to_string()));
    }

    Ok(text.trim_end_matches('/').to_string())
}
