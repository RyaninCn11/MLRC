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

/// What a version is, for grouping and filtering.
///
/// The manifest only knows `release`, `snapshot`, `old_beta` and `old_alpha`. April
/// Fools builds are snapshots that carry no marker of their own, so they get a
/// category derived from the data: a snapshot released on 1 April, plus the few that
/// shipped on 31 March (`1.RV-Pre1`) and are named here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Release,
    Snapshot,
    OldBeta,
    OldAlpha,
    AprilFools,
}

/// Snapshots that are April Fools builds but not dated 1 April.
const APRIL_FOOLS_IDS: [&str; 1] = ["1.RV-Pre1"];

impl Channel {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "release" | "releases" | "stable" => Some(Self::Release),
            "snapshot" | "snapshots" => Some(Self::Snapshot),
            "old_beta" | "old-beta" | "beta" | "oldbeta" => Some(Self::OldBeta),
            "old_alpha" | "old-alpha" | "alpha" | "oldalpha" => Some(Self::OldAlpha),
            "april_fools" | "april-fools" | "aprilfools" | "fool" => Some(Self::AprilFools),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Snapshot => "snapshot",
            Self::OldBeta => "old_beta",
            Self::OldAlpha => "old_alpha",
            Self::AprilFools => "april_fools",
        }
    }

    /// Every category, in the order a listing should show them.
    pub const ALL: [Self; 5] = [
        Self::Release,
        Self::Snapshot,
        Self::OldBeta,
        Self::OldAlpha,
        Self::AprilFools,
    ];
}

impl VersionEntry {
    pub fn channel(&self) -> Channel {
        if self.is_april_fools() {
            return Channel::AprilFools;
        }

        match self.kind.as_str() {
            "release" => Channel::Release,
            "old_beta" => Channel::OldBeta,
            "old_alpha" => Channel::OldAlpha,
            _ => Channel::Snapshot,
        }
    }

    fn is_april_fools(&self) -> bool {
        if APRIL_FOOLS_IDS.contains(&self.id.as_str()) {
            return true;
        }
        if self.kind != "snapshot" {
            return false;
        }
        // `releaseTime` is RFC 3339; the day is everything before the `T`.
        self.release_time
            .split('T')
            .next()
            .map(|date| date.ends_with("-04-01"))
            .unwrap_or(false)
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
