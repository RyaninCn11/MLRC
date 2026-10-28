//! Asset index.
//!
//! The index maps a logical name (`minecraft/sounds/...`) to a content hash. The
//! bytes live on `resources.download.minecraft.net` under that hash, which is why
//! every Minecraft install on a machine can share one object pool.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::port::DataRoot;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssetIndexJson {
    #[serde(default)]
    pub objects: BTreeMap<String, AssetObject>,
    /// Pre-1.7 indexes lay assets out by name instead of by hash.
    #[serde(rename = "virtual", default)]
    pub virtual_assets: Option<bool>,
    #[serde(default)]
    pub map_to_resources: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssetObject {
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub size: u64,
}

impl AssetIndexJson {
    pub fn parse(bytes: &[u8], id: &str) -> crate::error::MlrcResult<Self> {
        serde_json::from_slice(bytes).map_err(|err| {
            crate::error::MlrcError::new(
                crate::error::ErrorCode::MetaShape,
                format!("cannot parse asset index {id}: {err}"),
            )
            .with("asset_index", id)
        })
    }

    pub fn index_path(root: &DataRoot, id: &str) -> PathBuf {
        root.assets_dir().join("indexes").join(format!("{id}.json"))
    }
}

/// On-disk layout matches the official launcher: `objects/<first two>/<full hash>`.
pub fn object_path(root: &DataRoot, hash: &str) -> PathBuf {
    let prefix = &hash[..hash.len().min(2)];
    root.assets_dir().join("objects").join(prefix).join(hash)
}

/// The CDN path is `/<first two chars>/<full hash>` - the file name repeats the
/// prefix, it is not stripped. Getting this wrong yields a 404 for every object.
pub fn object_url(hash: &str) -> String {
    let prefix = &hash[..hash.len().min(2)];
    format!("https://resources.download.minecraft.net/{prefix}/{hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_url_repeats_the_prefix() {
        let hash = "f00657542252858a721e715a2e888a9226404e35";
        assert_eq!(
            object_url(hash),
            "https://resources.download.minecraft.net/f0/f00657542252858a721e715a2e888a9226404e35"
        );
    }
}
