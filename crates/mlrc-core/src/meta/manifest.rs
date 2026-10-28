//! `version_manifest_v2.json`, cached for an hour.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;
use crate::meta::MANIFEST_URL;
use crate::port::DataRoot;

const MAX_AGE: Duration = Duration::from_secs(3600);
const FILE_NAME: &str = "version_manifest_v2.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub release_time: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub compliance_level: u32,
}

impl VersionManifest {
    pub fn cache_path(root: &DataRoot) -> PathBuf {
        root.meta_dir().join(FILE_NAME)
    }

    /// Cache first, network second. A cached copy older than an hour is ignored.
    pub async fn load(client: &HttpClient, root: &DataRoot) -> MlrcResult<Self> {
        let path = Self::cache_path(root);
        if let Some(manifest) = read_cache(&path) {
            return Ok(manifest);
        }

        let manifest: Self = client.get_json(MANIFEST_URL).await?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(&manifest).map_err(|err| {
            MlrcError::new(ErrorCode::Io, format!("cannot serialize manifest: {err}"))
        })?;
        std::fs::write(&path, text)?;

        Ok(manifest)
    }

    pub fn find(&self, id: &str) -> Option<&VersionEntry> {
        self.versions.iter().find(|version| version.id == id)
    }
}

fn read_cache(path: &Path) -> Option<VersionManifest> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age >= MAX_AGE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
