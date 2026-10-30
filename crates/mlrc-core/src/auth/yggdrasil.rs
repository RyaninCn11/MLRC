//! Yggdrasil login, the protocol third-party authentication servers speak.
//!
//! The launcher exchanges a name and password for a token; the game is then started
//! with that token and with authlib-injector, which redirects the client's own session
//! requests to the same server. Without the injector the game would talk to Mojang, and
//! an online-mode server on the third-party service would reject the join.
//!
//! The endpoints are the ones every server implements, and the shapes below are only
//! the fields this launcher reads: servers add their own keys freely.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::client::HttpClient;
use crate::fetch::mirror::Mirror;
use crate::port::DataRoot;

const AGENT_NAME: &str = "Minecraft";
const AGENT_VERSION: u32 = 1;

/// What `mlrc auth login` produces and `mlrc launch` consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// API root the account belongs to.
    pub server: String,
    /// The email (or, on some servers, the name) used to log in. Only for display.
    pub username: String,
    /// In-game name.
    pub name: String,
    /// Dashed UUID.
    pub uuid: String,
    pub access_token: String,
    pub client_token: String,
}

impl Session {
    /// The identity the game is started with.
    pub fn identity(&self) -> super::Identity {
        super::Identity {
            name: self.name.clone(),
            uuid: self.uuid.clone(),
            access_token: self.access_token.clone(),
            // The official client sends `mojang` for a token-backed session; the
            // value is what the client tells the server it is.
            user_type: "mojang".to_string(),
            xuid: String::new(),
            client_id: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credentials {
    access_token: String,
    #[serde(default)]
    client_token: String,
    #[serde(default)]
    selected_profile: Option<Profile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YggdrasilError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_message: String,
}

impl Session {
    pub fn file(root: &DataRoot) -> PathBuf {
        root.auth_path()
    }

    pub fn load(root: &DataRoot) -> MlrcResult<Option<Self>> {
        read(&Self::file(root))
    }

    pub fn save(&self, root: &DataRoot) -> MlrcResult<()> {
        write(&Self::file(root), self)
    }

    pub fn remove(root: &DataRoot) -> MlrcResult<bool> {
        match std::fs::remove_file(Self::file(root)) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot remove {}: {err}", Self::file(root).display()),
            )),
        }
    }
}

/// Logs in and returns the session, with a fresh client token when none is given.
pub async fn authenticate(
    client: &HttpClient,
    server: &str,
    username: &str,
    password: &str,
    client_token: Option<&str>,
) -> MlrcResult<Session> {
    let client_token = match client_token {
        Some(token) if !token.is_empty() => token.to_string(),
        _ => new_token(),
    };

    let body = serde_json::json!({
        "username": username,
        "password": password,
        "clientToken": client_token,
        "agent": { "name": AGENT_NAME, "version": AGENT_VERSION },
        "requestUser": false,
    });

    let credentials: Credentials = post(client, server, "authserver/authenticate", &body).await?;
    let profile = credentials.selected_profile.ok_or_else(|| {
        MlrcError::new(
            ErrorCode::AuthFailed,
            format!("{server} accepted the credentials but returned no profile"),
        )
        .with("server", server.to_string())
    })?;

    Ok(Session {
        server: server.to_string(),
        username: username.to_string(),
        name: profile.name,
        uuid: with_dashes(&profile.id),
        access_token: credentials.access_token,
        client_token: if credentials.client_token.is_empty() {
            client_token
        } else {
            credentials.client_token
        },
    })
}

/// Exchanges a token for a new one.
///
/// Servers mark the previous token "temporarily invalid" after a refresh; a launcher
/// that skips this step keeps a token that the game can use but that the server may
/// stop accepting after a while.
pub async fn refresh(client: &HttpClient, session: &Session) -> MlrcResult<Session> {
    let body = serde_json::json!({
        "accessToken": session.access_token,
        "clientToken": session.client_token,
    });

    let credentials: Credentials =
        post(client, &session.server, "authserver/refresh", &body).await?;

    Ok(Session {
        server: session.server.clone(),
        username: session.username.clone(),
        name: credentials
            .selected_profile
            .as_ref()
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| session.name.clone()),
        uuid: credentials
            .selected_profile
            .as_ref()
            .map(|profile| with_dashes(&profile.id))
            .unwrap_or_else(|| session.uuid.clone()),
        access_token: credentials.access_token,
        client_token: if credentials.client_token.is_empty() {
            session.client_token.clone()
        } else {
            credentials.client_token
        },
    })
}

/// Asks the server whether the token is still good. A network failure is reported as
/// `Ok(None)`: the caller can still start the game offline.
pub async fn validate(client: &HttpClient, session: &Session) -> MlrcResult<Option<bool>> {
    let body = serde_json::json!({
        "accessToken": session.access_token,
        "clientToken": session.client_token,
    });

    match post_raw(client, &session.server, "authserver/validate", &body).await {
        Ok((status, _)) => Ok(Some(status < 400)),
        Err(err) if err.code == ErrorCode::Network => Ok(None),
        Err(err) => Err(err),
    }
}

/// Revokes the token server-side. Failure is not fatal: the local copy is dropped
/// either way, and an expired token is harmless.
pub async fn invalidate(client: &HttpClient, session: &Session) -> MlrcResult<()> {
    let body = serde_json::json!({
        "accessToken": session.access_token,
        "clientToken": session.client_token,
    });

    match post_raw(client, &session.server, "authserver/invalidate", &body).await {
        Ok(_) => Ok(()),
        Err(err) if err.code == ErrorCode::Network => Ok(()),
        Err(err) => Err(err),
    }
}

async fn post<T: for<'de> Deserialize<'de>>(
    client: &HttpClient,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> MlrcResult<T> {
    let (status, bytes) = post_raw(client, server, path, body).await?;

    if status >= 400 {
        return Err(failure(server, status, &bytes));
    }

    serde_json::from_slice(&bytes).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("cannot parse the answer of {server}/{path}: {err}"),
        )
        .with("server", server.to_string())
    })
}

async fn post_raw(
    client: &HttpClient,
    server: &str,
    path: &str,
    body: &serde_json::Value,
) -> MlrcResult<(u16, Vec<u8>)> {
    let url = format!("{}/{path}", server.trim_end_matches('/'));
    client.post_json_raw(&url, body).await
}

/// Turns a Yggdrasil error body into an explanation.
///
/// The status alone is not enough: every failure is a 403, and `errorMessage` is the
/// only place that says whether the password was wrong or the token expired.
fn failure(server: &str, status: u16, bytes: &[u8]) -> MlrcError {
    let detail: YggdrasilError = serde_json::from_slice(bytes).unwrap_or(YggdrasilError {
        error: String::new(),
        error_message: String::new(),
    });

    let reason = if detail.error_message.is_empty() {
        format!("http {status}")
    } else {
        detail.error_message
    };

    let mut error =
        MlrcError::new(ErrorCode::AuthFailed, reason).with("server", server.to_string());
    if !detail.error.is_empty() {
        error = error.with("reason", detail.error);
    }
    error
}

/// A UUID-shaped random string. Not security-sensitive: the server only uses it to
/// tell this launcher's sessions apart, and 128 random bits suffice for that.
pub fn new_token() -> String {
    let mut hex = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        hex.push_str(&format!("{:016x}", hasher.finish()));
    }
    hex.truncate(32);
    hex
}

fn with_dashes(id: &str) -> String {
    let clean: String = id.chars().filter(|ch| *ch != '-').collect();
    if clean.len() != 32 {
        return id.to_string();
    }
    format!(
        "{}-{}-{}-{}-{}",
        &clean[0..8],
        &clean[8..12],
        &clean[12..16],
        &clean[16..20],
        &clean[20..32]
    )
}

fn read(path: &Path) -> MlrcResult<Option<Session>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let session = serde_json::from_str(&text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Config,
                    format!("invalid {}: {err}", path.display()),
                )
            })?;
            Ok(Some(session))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(MlrcError::new(
            ErrorCode::Io,
            format!("cannot read {}: {err}", path.display()),
        )),
    }
}

fn write(path: &Path, session: &Session) -> MlrcResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let text = serde_json::to_string_pretty(session)
        .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))?;
    std::fs::write(path, text).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot write {}: {err}", path.display()),
        )
    })?;

    // The file holds a token: make it readable by the owner only where the platform
    // has the notion. On Windows the account's ACL already keeps it private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Where authlib-injector lives, and the JVM argument that installs it.
pub struct Injector {
    pub path: PathBuf,
    pub build: u64,
}

impl Injector {
    pub fn argument(&self, server: &str) -> String {
        format!("-javaagent:{}={server}", self.path.display())
    }
}

#[derive(Debug, Deserialize)]
struct Latest {
    build_number: u64,
    download_url: String,
    checksums: Checksums,
}

#[derive(Debug, Deserialize)]
struct Checksums {
    sha256: String,
}

const LATEST_URL: &str = "https://authlib-injector.yushi.moe/artifact/latest.json";
const LATEST_URL_MIRROR: &str =
    "https://bmclapi2.bangbang93.com/mirrors/authlib-injector/artifact/latest.json";

/// The newest injector already on disk, if any.
///
/// Launch consults this first: after `mlrc auth login` has fetched the jar once, no
/// launch needs the network for it, and an offline launch keeps working.
pub fn cached_injector(root: &DataRoot) -> MlrcResult<Option<Injector>> {
    let dir = root.injector_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", dir.display()),
            ))
        }
    };

    let mut best: Option<Injector> = None;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(build) = name
            .strip_prefix("authlib-injector-")
            .and_then(|rest| rest.strip_suffix(".jar"))
            .and_then(|digits| digits.parse::<u64>().ok())
        else {
            continue;
        };

        let better = best
            .as_ref()
            .map(|current| build > current.build)
            .unwrap_or(true);
        if better {
            best = Some(Injector {
                path: entry.path(),
                build,
            });
        }
    }

    Ok(best)
}

/// Downloads authlib-injector if this build is not on disk yet, and verifies it.
///
/// The jar is what makes the game talk to the third-party server, so it is a hard
/// requirement whenever a server is configured; a launcher that silently skipped it
/// would produce a client that logs in and then cannot join anything.
pub async fn ensure_injector(
    client: &HttpClient,
    root: &DataRoot,
    mirror: Mirror,
) -> MlrcResult<Injector> {
    if let Some(found) = cached_injector(root)? {
        return Ok(found);
    }

    let url = if mirror == Mirror::Bmclapi {
        LATEST_URL_MIRROR
    } else {
        LATEST_URL
    };

    let latest: Latest = client.get_json(url).await?;
    let dir = root.injector_dir();
    std::fs::create_dir_all(&dir)?;

    let path = dir.join(format!("authlib-injector-{}.jar", latest.build_number));
    let injector = Injector {
        path,
        build: latest.build_number,
    };

    // A previous run may have fetched exactly this build; a stale or truncated file
    // fails the checksum and is simply downloaded again.
    let expected = latest.checksums.sha256.to_ascii_lowercase();
    if injector.path.is_file() && sha256_matches(&injector.path, &expected)? {
        return Ok(injector);
    }

    let bytes = client.get_bytes(&latest.download_url).await?;
    let actual = sha256_hex(&bytes);
    if !expected.is_empty() && actual != expected {
        return Err(MlrcError::new(
            ErrorCode::HashMismatch,
            format!(
                "authlib-injector {} failed its checksum",
                latest.build_number
            ),
        )
        .with("expected", expected)
        .with("actual", actual));
    }

    std::fs::write(&injector.path, &bytes)?;
    Ok(injector)
}

fn sha256_matches(path: &Path, expected: &str) -> MlrcResult<bool> {
    if expected.is_empty() {
        return Ok(true);
    }
    let bytes = std::fs::read(path).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot read {}: {err}", path.display()),
        )
    })?;
    Ok(sha256_hex(&bytes) == expected)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_get_their_dashes_back() {
        assert_eq!(
            with_dashes("b50ad385829d3141a2167e7d7539ba7f"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        // Already dashed input survives unchanged.
        assert_eq!(
            with_dashes("b50ad385-829d-3141-a216-7e7d7539ba7f"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
    }

    #[test]
    fn tokens_are_unique_and_32_hex_characters() {
        let first = new_token();
        let second = new_token();
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn failure_body_becomes_the_message() {
        let error = failure(
            "https://example.invalid/api/yggdrasil",
            403,
            br#"{"error":"ForbiddenOperationException","errorMessage":"Invalid credentials. Invalid username or password."}"#,
        );
        assert_eq!(error.code, ErrorCode::AuthFailed);
        assert_eq!(
            error.message,
            "Invalid credentials. Invalid username or password."
        );
    }
}
