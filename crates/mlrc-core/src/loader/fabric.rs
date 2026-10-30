//! Fabric and Quilt loader profiles.
//!
//! Both publish, for every loader/Minecraft pair, a version document that declares
//! `inheritsFrom` and adds its own libraries. Resolving one is two requests: the
//! available loaders, then the profile document. Only the host and the API version in
//! the path differ.

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;
use crate::meta::version::VersionJson;

pub const META_BASE: &str = "https://meta.fabricmc.net";
pub const QUILT_META_BASE: &str = "https://meta.quiltmc.org";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Fabric,
    Quilt,
}

impl Family {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "fabric" => Some(Self::Fabric),
            "quilt" => Some(Self::Quilt),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
        }
    }

    fn base(self) -> &'static str {
        match self {
            Self::Fabric => META_BASE,
            Self::Quilt => QUILT_META_BASE,
        }
    }

    /// Fabric serves `/v2`, Quilt `/v3`; the payloads have the same shape.
    fn api(self) -> &'static str {
        match self {
            Self::Fabric => "v2",
            Self::Quilt => "v3",
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoaderEntry {
    loader: LoaderVersion,
}

#[derive(Debug, Deserialize)]
struct LoaderVersion {
    version: String,
    #[serde(default)]
    stable: bool,
}

pub struct LoaderProfile {
    /// The id the loader itself gives the profile, e.g. `quilt-loader-0.27.0-1.21.1`.
    pub id: String,
    pub loader: String,
    pub version: VersionJson,
    /// Raw bytes, kept so the document can be cached verbatim for launch.
    pub document: Vec<u8>,
}

/// Kept for call sites that only ever wanted Fabric.
pub type FabricProfile = LoaderProfile;

pub async fn resolve(
    client: &HttpClient,
    mc: &str,
    requested: Option<&str>,
) -> MlrcResult<FabricProfile> {
    resolve_family(client, Family::Fabric, mc, requested).await
}

#[derive(Debug, Deserialize)]
struct GameEntry {
    version: String,
}

/// Every Minecraft version the loader has a build for, newest first.
///
/// One request, not one per version: the meta API publishes the whole list. It
/// includes snapshots and old alphas, so callers filter it against the manifest.
pub async fn supported_game_versions(
    client: &HttpClient,
    family: Family,
) -> MlrcResult<Vec<String>> {
    let url = format!("{}/{}/versions/game", family.base(), family.api());
    let entries: Vec<GameEntry> = client.get_json(&url).await?;
    Ok(entries.into_iter().map(|entry| entry.version).collect())
}

pub async fn resolve_family(
    client: &HttpClient,
    family: Family,
    mc: &str,
    requested: Option<&str>,
) -> MlrcResult<LoaderProfile> {
    let loader = match requested {
        Some(version) => version.to_string(),
        None => latest(client, family, mc).await?,
    };

    let url = format!(
        "{}/{}/versions/loader/{mc}/{loader}/profile/json",
        family.base(),
        family.api()
    );
    let document = client.get_bytes(&url).await.map_err(|err| {
        if is_not_found(&err) {
            unavailable(family, mc, &loader)
        } else {
            err
        }
    })?;

    let version: VersionJson = serde_json::from_slice(&document).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!(
                "cannot parse the {} profile for {mc}: {err}",
                family.as_str()
            ),
        )
        .with("version", mc)
    })?;

    Ok(LoaderProfile {
        id: version.id.clone(),
        loader,
        version,
        document,
    })
}

/// Loaders come newest first; prefer a stable one.
async fn latest(client: &HttpClient, family: Family, mc: &str) -> MlrcResult<String> {
    let url = format!("{}/{}/versions/loader/{mc}", family.base(), family.api());
    let entries: Vec<LoaderEntry> = client.get_json(&url).await.map_err(|err| {
        if is_not_found(&err) {
            unavailable(family, mc, "stable")
        } else {
            err
        }
    })?;

    // Fabric marks stable releases; Quilt only says so in the version string.
    entries
        .iter()
        .find(|entry| entry.loader.stable)
        .or_else(|| {
            entries.iter().find(|entry| {
                let version = entry.loader.version.to_ascii_lowercase();
                !version.contains("-beta") && !version.contains("-alpha")
            })
        })
        .or_else(|| entries.first())
        .map(|entry| entry.loader.version.clone())
        .ok_or_else(|| unavailable(family, mc, "stable"))
}

fn unavailable(family: Family, mc: &str, loader: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::LoaderUnavailable,
        format!(
            "{} has no loader {loader} for minecraft {mc}",
            family.as_str()
        ),
    )
    .with("minecraft", mc.to_string())
    .with("loader", loader.to_string())
}

fn is_not_found(err: &MlrcError) -> bool {
    err.context.get("status").and_then(|value| value.as_u64()) == Some(404)
}
