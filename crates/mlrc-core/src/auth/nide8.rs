//! Unified Pass (Nide8): a third-party authentication service for Minecraft servers.
//!
//! The protocol is the Yggdrasil one, so the login itself reuses
//! [`crate::auth::yggdrasil`]; what is different is where the endpoints come from and
//! what the game needs at launch:
//!
//! - a server is identified by a 32 character id the server owner hands out;
//! - `https://auth.mc-user.com:233/<id>/` answers with a document whose `apiRoot` is the
//!   real base url, plus the server's display name and the hash of its client component;
//! - the game must be started with `-javaagent:nide8auth.jar=<id>` and
//!   `-Dnide8auth.client=true`, or it talks to Mojang and cannot join that server.
//!
//! Passwords are never stored here. The token pair that Yggdrasil returns is enough to
//! refresh and validate, and a password on disk is a liability with no benefit.

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;

/// Where every server id starts. The trailing slash matters: everything below is
/// appended to it.
pub const BASE: &str = "https://auth.mc-user.com:233";

/// The registration page for a server, for a front-end to open in a browser.
pub fn register_url(server_id: &str) -> String {
    format!("https://login.mc-user.com:233/{server_id}/loginreg")
}

/// A server is identified by exactly 32 hexadecimal characters.
pub fn validate_id(server_id: &str) -> MlrcResult<&str> {
    let id = server_id.trim();
    if id.len() == 32 && id.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(MlrcError::new(
            ErrorCode::Usage,
            format!("invalid unified pass server id: {id} (expected 32 hexadecimal characters)"),
        )
        .with("server_id", id.to_string()))
    }
}

/// What the discovery document tells us.
#[derive(Debug, Clone, Default)]
pub struct Server {
    pub id: String,
    /// The base every auth request goes to, after `apiRoot` was applied.
    pub api_root: String,
    pub name: String,
    pub ip: String,
    /// SHA1 of the client component, for an update check.
    pub jar_sha1: String,
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    meta: Meta,
    /// Some deployments answer with a relative root; see `resolve`.
    #[serde(default, rename = "apiRoot")]
    api_root: String,
    #[serde(default, rename = "jarHash")]
    jar_hash: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default)]
    server_name: String,
    #[serde(default, rename = "serverIP")]
    server_ip: String,
}

/// Fetches the server document and resolves the base url.
///
/// The document is the only source of the real endpoint: a server owner can move it,
/// and the id-based url keeps working as a directory to whatever the current root is.
pub async fn discover(client: &HttpClient, server_id: &str) -> MlrcResult<Server> {
    let id = validate_id(server_id)?;
    let url = format!("{BASE}/{id}/");
    let document: Document = client.get_json(&url).await?;

    Ok(Server {
        id: id.to_string(),
        api_root: resolve(&url, &document.api_root),
        name: document.meta.server_name,
        ip: document.meta.server_ip,
        jar_sha1: document.jar_hash.to_ascii_lowercase(),
    })
}

/// `apiRoot` is documented as a value to assign to the base url, but not as absolute.
/// An empty value means "this directory"; a relative one is joined onto it.
fn resolve(base: &str, api_root: &str) -> String {
    let root = api_root.trim();
    let joined = if root.is_empty() {
        base.to_string()
    } else if root.starts_with("http://") || root.starts_with("https://") {
        root.to_string()
    } else {
        let relative = root.trim_start_matches('/');
        if relative.is_empty() {
            base.to_string()
        } else {
            format!("{base}{relative}")
        }
    };

    let trimmed = joined.trim_end_matches('/');
    format!("{trimmed}/")
}

/// The two JVM arguments the client component needs.
///
/// The jar path is the caller's: a launcher may ship it, download it, or let the user
/// point at it. Relative is used when the path contains `=` because Java splits the
/// agent argument on the first `=` - an absolute Windows path can contain one.
pub fn java_arguments(
    jar: &std::path::Path,
    server_id: &str,
    cwd: &std::path::Path,
) -> Vec<String> {
    let path = if jar.to_string_lossy().contains('=') {
        jar.strip_prefix(cwd).unwrap_or(jar).to_path_buf()
    } else {
        jar.to_path_buf()
    };

    vec![
        format!("-javaagent:{}={server_id}", path.display()),
        "-Dnide8auth.client=true".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_id_is_32_hex_characters() {
        assert!(validate_id("1234567890abcdef1234567890abcdef").is_ok());
        assert!(validate_id(" 1234567890ABCDEF1234567890ABCDEF ").is_ok());
        assert!(validate_id("1234").is_err());
        assert!(validate_id("1234567890abcdef1234567890abcdeg").is_err());
    }

    #[test]
    fn the_api_root_is_absolute_relative_or_absent() {
        let base = "https://auth.mc-user.com:233/abc/";
        assert_eq!(
            resolve(base, "https://example.test/api/"),
            "https://example.test/api/"
        );
        assert_eq!(
            resolve(base, "/api"),
            "https://auth.mc-user.com:233/abc/api/"
        );
        assert_eq!(resolve(base, ""), base);
    }

    #[test]
    fn the_agent_argument_avoids_equals_signs() {
        let cwd = std::path::Path::new("C:/games/1.21.1");
        let plain = std::path::Path::new("C:/mlrc/nide8auth.jar");
        assert_eq!(
            java_arguments(plain, "abc", cwd),
            vec![
                "-javaagent:C:/mlrc/nide8auth.jar=abc".to_string(),
                "-Dnide8auth.client=true".to_string()
            ]
        );

        // A path with `=` is passed relative to the game directory instead.
        let awkward_cwd = std::path::Path::new("C:/ga=mes/1.21.1");
        let awkward = std::path::Path::new("C:/ga=mes/1.21.1/nide8auth.jar");
        let arguments = java_arguments(awkward, "abc", awkward_cwd);
        assert_eq!(arguments[0], "-javaagent:nide8auth.jar=abc");
    }

    #[test]
    fn the_registration_page_is_derived_from_the_id() {
        assert_eq!(
            register_url("1234567890abcdef1234567890abcdef"),
            "https://login.mc-user.com:233/1234567890abcdef1234567890abcdef/loginreg"
        );
    }
}
