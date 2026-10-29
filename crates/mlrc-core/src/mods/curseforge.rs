//! CurseForge, the "Eternal" API.
//!
//! Two things shape this module:
//!
//! * The API needs a key, and a key belongs to whoever registered it. MLRC never
//!   ships one: `curseforge.api_key` or `MLRC_CURSEFORGE_KEY` supplies it.
//! * Authors may forbid third-party downloads. Such a file comes back without a
//!   `downloadUrl`, and the launcher has to say so instead of failing obscurely.
//!
//! The search endpoint is not used: it answers 403 for keys that are allowed to read
//! mods and files, so lookups start from a project id.

use std::time::Duration;

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;

const BASE: &str = "https://api.curseforge.com/v1";

#[derive(Debug, Clone)]
pub struct CfFile {
    pub mod_id: u64,
    pub file_id: u64,
    pub display_name: String,
    pub file_name: String,
    pub download_url: String,
    pub sha1: String,
    pub size: u64,
    /// 1 release, 2 beta, 3 alpha.
    pub release_type: u32,
    /// Mod ids of `required` dependencies.
    pub required: Vec<u64>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Mod {
    /// `Some(false)` when the author forbids third-party downloads.
    #[serde(default, rename = "allowModDistribution")]
    allow_mod_distribution: Option<bool>,
}

#[derive(Deserialize)]
struct FileEntry {
    id: u64,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default, rename = "fileName")]
    file_name: String,
    #[serde(default, rename = "releaseType")]
    release_type: u32,
    #[serde(default, rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default, rename = "fileLength")]
    file_length: u64,
    #[serde(default)]
    hashes: Vec<Hash>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Hash {
    #[serde(default)]
    value: String,
    #[serde(default)]
    algo: u32,
}

#[derive(Deserialize)]
struct Dependency {
    #[serde(default, rename = "modId")]
    mod_id: u64,
    /// 1 embedded, 2 optional, 3 required, 4 tool, 5 incompatible, 6 include.
    #[serde(default, rename = "relationType")]
    relation_type: u32,
}

impl FileEntry {
    fn sha1(&self) -> String {
        self.hashes
            .iter()
            .find(|hash| hash.algo == 1)
            .map(|hash| hash.value.clone())
            .unwrap_or_default()
    }
}

/// Modrinth's loader names mapped to CurseForge's loader ids.
fn loader_type(loader: &str) -> Option<u32> {
    match loader {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

fn headers(key: &str) -> Vec<(&'static str, String)> {
    vec![
        ("x-api-key", key.to_string()),
        ("Accept", "application/json".to_string()),
    ]
}

/// Resolves the newest file of `project` for a game version and loader.
///
/// `project` is a numeric mod id; a slug is tried first as an id and then rejected
/// with a hint, because the slug endpoint does not exist.
pub async fn resolve(
    client: &HttpClient,
    key: &str,
    project: &str,
    mc: &str,
    loader: &str,
) -> MlrcResult<CfFile> {
    let project = project.trim();
    let mod_id: u64 = project.parse().map_err(|_| {
        MlrcError::new(
            ErrorCode::Usage,
            format!(
                "curseforge needs a numeric project id, not {project}; \
                 open curseforge.com/minecraft/mc-mods/<name> and copy the id from the URL of its API page"
            ),
        )
        .with("project", project.to_string())
    })?;

    let loader = loader_type(loader).ok_or_else(|| {
        MlrcError::new(
            ErrorCode::Usage,
            format!("curseforge has no loader type for {loader}"),
        )
    })?;

    let url = format!("{BASE}/mods/{mod_id}/files");
    let files: Envelope<Vec<FileEntry>> = client
        .get_json_with_headers(
            &url,
            &[
                ("gameVersion", mc.to_string()),
                ("modLoaderType", loader.to_string()),
                ("pageSize", "50".to_string()),
            ],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    if files.data.is_empty() {
        return Err(MlrcError::new(
            ErrorCode::ModNotFound,
            format!("no curseforge file for {mc} on {loader} (mod {mod_id})"),
        )
        .with("mod_id", mod_id));
    }

    // Installing takes a release when one exists; a beta only if there is nothing else.
    let mod_info: Envelope<Mod> = client
        .get_json_with_headers(
            &format!("{BASE}/mods/{mod_id}"),
            &[],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    let chosen = files
        .data
        .iter()
        .find(|file| file.release_type == 1)
        .unwrap_or(&files.data[0]);

    finish(client, key, mod_id, chosen, &mod_info.data).await
}

/// Every file of a project for a game version and loader, newest first.
///
/// `mod update` needs the list rather than one pick: whether something is newer is a
/// question of file ids, not of release type. Choosing only releases would "update" a
/// beta the pack pinned back down to an older release.
pub async fn files(
    client: &HttpClient,
    key: &str,
    project: &str,
    mc: &str,
    loader: &str,
) -> MlrcResult<Vec<CfFile>> {
    let mod_id: u64 = project.trim().parse().map_err(|_| {
        MlrcError::new(
            ErrorCode::Usage,
            format!(
                "curseforge needs a numeric project id, not {}",
                project.trim()
            ),
        )
    })?;

    let loader = loader_type(loader).ok_or_else(|| {
        MlrcError::new(
            ErrorCode::Usage,
            format!("curseforge has no loader type for {loader}"),
        )
    })?;

    let listed: Envelope<Vec<FileEntry>> = client
        .get_json_with_headers(
            &format!("{BASE}/mods/{mod_id}/files"),
            &[
                ("gameVersion", mc.to_string()),
                ("modLoaderType", loader.to_string()),
                ("pageSize", "50".to_string()),
            ],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    let mod_info: Envelope<Mod> = client
        .get_json_with_headers(
            &format!("{BASE}/mods/{mod_id}"),
            &[],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    let mut out = Vec::new();
    for entry in &listed.data {
        // Files without a usable link are skipped instead of failing the whole list.
        if let Ok(file) = finish(client, key, mod_id, entry, &mod_info.data).await {
            out.push(file);
        }
    }
    Ok(out)
}

async fn finish(
    client: &HttpClient,
    key: &str,
    mod_id: u64,
    entry: &FileEntry,
    info: &Mod,
) -> MlrcResult<CfFile> {
    if info.allow_mod_distribution == Some(false) {
        return Err(restricted(mod_id, &entry.file_name));
    }

    let download_url = match &entry.download_url {
        Some(url) => url.clone(),
        None => download_link(client, key, mod_id, entry.id).await?,
    };

    Ok(CfFile {
        mod_id,
        file_id: entry.id,
        display_name: entry.display_name.clone(),
        file_name: entry.file_name.clone(),
        download_url,
        sha1: entry.sha1(),
        size: entry.file_length,
        release_type: entry.release_type,
        required: entry
            .dependencies
            .iter()
            .filter(|dependency| dependency.relation_type == 3)
            .map(|dependency| dependency.mod_id)
            .collect(),
    })
}

/// One exact file, as a modpack's `manifest.json` names it.
pub async fn file(client: &HttpClient, key: &str, mod_id: u64, file_id: u64) -> MlrcResult<CfFile> {
    let entry: Envelope<FileEntry> = client
        .get_json_with_headers(
            &format!("{BASE}/mods/{mod_id}/files/{file_id}"),
            &[],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    let mod_info: Envelope<Mod> = client
        .get_json_with_headers(
            &format!("{BASE}/mods/{mod_id}"),
            &[],
            &headers(key),
            Duration::from_secs(30),
        )
        .await
        .map_err(|err| explain(err, mod_id))?;

    if mod_info.data.allow_mod_distribution == Some(false) {
        return Err(restricted(mod_id, &entry.data.file_name));
    }

    let download_url = match &entry.data.download_url {
        Some(url) => url.clone(),
        None => download_link(client, key, mod_id, file_id).await?,
    };

    Ok(CfFile {
        mod_id,
        file_id,
        display_name: entry.data.display_name.clone(),
        file_name: entry.data.file_name.clone(),
        download_url,
        sha1: entry.data.sha1(),
        size: entry.data.file_length,
        release_type: entry.data.release_type,
        required: Vec::new(),
    })
}

/// Asks the dedicated endpoint, which is the documented way once `downloadUrl` is null.
async fn download_link(
    client: &HttpClient,
    key: &str,
    mod_id: u64,
    file_id: u64,
) -> MlrcResult<String> {
    let url = format!("{BASE}/mods/{mod_id}/files/{file_id}/download-url");
    let response: Envelope<Option<String>> = client
        .get_json_with_headers(&url, &[], &headers(key), Duration::from_secs(30))
        .await
        .map_err(|err| explain(err, mod_id))?;

    response
        .data
        .filter(|link| !link.is_empty())
        .ok_or_else(|| restricted(mod_id, &file_id.to_string()))
}

fn restricted(mod_id: u64, file: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::ModRestricted,
        format!(
            "curseforge mod {mod_id} ({file}) does not allow third-party downloads; \
             download it from curseforge.com and drop it into mods/"
        ),
    )
    .with("mod_id", mod_id)
}

fn explain(err: MlrcError, mod_id: u64) -> MlrcError {
    if err.code == ErrorCode::Network && err.message.contains("403") {
        return MlrcError::new(
            ErrorCode::Usage,
            format!(
                "curseforge rejected the api key (403) for mod {mod_id}; \
                 check `mlrc config set curseforge.api_key <key>`"
            ),
        )
        .with("mod_id", mod_id);
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_ids_match_the_api() {
        assert_eq!(loader_type("forge"), Some(1));
        assert_eq!(loader_type("fabric"), Some(4));
        assert_eq!(loader_type("quilt"), Some(5));
        assert_eq!(loader_type("neoforge"), Some(6));
        assert_eq!(loader_type("vanilla"), None);
    }
}
