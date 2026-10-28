//! Fabric loader profiles.
//!
//! Fabric publishes, for every loader/Minecraft pair, a version document that
//! declares `inheritsFrom` and adds its own libraries. Resolving one is two requests:
//! the available loaders, then the profile document.

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;
use crate::meta::version::VersionJson;

pub const META_BASE: &str = "https://meta.fabricmc.net";

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

pub struct FabricProfile {
    /// The id Fabric itself gives the profile, e.g. `fabric-loader-0.16.9-1.21.1`.
    pub id: String,
    pub loader: String,
    pub version: VersionJson,
    /// Raw bytes, kept so the document can be cached verbatim for launch.
    pub document: Vec<u8>,
}

pub async fn resolve(
    client: &HttpClient,
    mc: &str,
    requested: Option<&str>,
) -> MlrcResult<FabricProfile> {
    let loader = match requested {
        Some(version) => version.to_string(),
        None => latest(client, mc).await?,
    };

    let url = format!("{META_BASE}/v2/versions/loader/{mc}/{loader}/profile/json");
    let document = client.get_bytes(&url).await.map_err(|err| {
        if is_not_found(&err) {
            unavailable(mc, &loader)
        } else {
            err
        }
    })?;

    let version: VersionJson = serde_json::from_slice(&document).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("cannot parse the fabric profile for {mc}: {err}"),
        )
        .with("version", mc)
    })?;

    Ok(FabricProfile {
        id: version.id.clone(),
        loader,
        version,
        document,
    })
}

/// Fabric returns loaders newest first; prefer a stable one.
async fn latest(client: &HttpClient, mc: &str) -> MlrcResult<String> {
    let url = format!("{META_BASE}/v2/versions/loader/{mc}");
    let entries: Vec<LoaderEntry> = client.get_json(&url).await.map_err(|err| {
        if is_not_found(&err) {
            unavailable(mc, "stable")
        } else {
            err
        }
    })?;

    entries
        .iter()
        .find(|entry| entry.loader.stable)
        .or_else(|| entries.first())
        .map(|entry| entry.loader.version.clone())
        .ok_or_else(|| unavailable(mc, "stable"))
}

fn unavailable(mc: &str, loader: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::LoaderUnavailable,
        format!("fabric has no loader {loader} for minecraft {mc}"),
    )
    .with("minecraft", mc.to_string())
    .with("loader", loader.to_string())
}

fn is_not_found(err: &MlrcError) -> bool {
    err.context.get("status").and_then(|value| value.as_u64()) == Some(404)
}
