//! Installing a Mojang Java runtime (`java-runtime-*`, `jre-legacy`).
//!
//! The runtime index is keyed by platform and component; each entry points at a
//! per-platform manifest listing every JDK file. We install into
//! `data/java/jdk-<major>` keyed by **major**, not by component: Mojang ships
//! several components for the same major and keying by component would download 17
//! twice.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::EventSink;
use crate::fetch::download::{DownloadTask, Downloader};
use crate::fetch::HttpClient;
use crate::java::discover::{self, JavaInstallation};
use crate::port::DataRoot;
use crate::store::ObjectStore;

pub const INDEX_URL: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
const INDEX_CACHE: &str = "java-runtime-index.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Deserialize)]
pub struct RuntimeIndex(pub BTreeMap<String, BTreeMap<String, Vec<RuntimeEntry>>>);

#[derive(Debug, Deserialize)]
pub struct RuntimeEntry {
    pub manifest: RuntimeManifestRef,
    #[serde(default)]
    pub version: RuntimeVersion,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeManifestRef {
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct RuntimeVersion {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeManifest {
    #[serde(default)]
    pub files: BTreeMap<String, RuntimeFile>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeFile {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub downloads: Option<RuntimeDownloads>,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeDownloads {
    #[serde(default)]
    pub raw: Option<RuntimeRaw>,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeRaw {
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
}

/// What the user asked for on `mlrc java install`.
pub enum Target<'a> {
    Component(&'a str),
    Major(u32),
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub component: String,
    pub major: u32,
    pub version: String,
    pub manifest_url: String,
}

pub fn platform_key() -> &'static str {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            "windows-x64"
        } else if cfg!(target_arch = "aarch64") {
            "windows-arm64"
        } else {
            "windows-x86"
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "mac-os-arm64"
        } else {
            "mac-os"
        }
    } else if cfg!(target_arch = "x86") {
        "linux-i386"
    } else {
        "linux"
    }
}

pub fn resolve(index: &RuntimeIndex, platform: &str, target: Target<'_>) -> Option<Resolved> {
    let components = index.0.get(platform)?;

    match target {
        Target::Component(name) => {
            let entry = components.get(name)?.first()?;
            Some(Resolved {
                component: name.to_string(),
                major: discover::parse_major(&entry.version.name)?,
                version: entry.version.name.clone(),
                manifest_url: entry.manifest.url.clone(),
            })
        }
        Target::Major(major) => {
            let best = components
                .iter()
                .filter(|(name, _)| !name.ends_with("-snapshot"))
                .filter_map(|(name, entries)| {
                    let entry = entries.first()?;
                    let entry_major = discover::parse_major(&entry.version.name)?;
                    (entry_major == major).then(|| {
                        (
                            name.clone(),
                            entry.version.name.clone(),
                            entry.manifest.url.clone(),
                        )
                    })
                })
                .max_by(|left, right| compare_versions(&left.1, &right.1))?;

            Some(Resolved {
                component: best.0,
                major,
                version: best.1,
                manifest_url: best.2,
            })
        }
    }
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left: Vec<u64> = left
        .split(['.', '_', '-', '+'])
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    let right: Vec<u64> = right
        .split(['.', '_', '-', '+'])
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    left.cmp(&right)
}

pub async fn load_index(client: &HttpClient, root: &DataRoot) -> MlrcResult<RuntimeIndex> {
    let path = root.meta_dir().join(INDEX_CACHE);

    if is_fresh(&path) {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(index) = serde_json::from_slice(&bytes) {
                return Ok(index);
            }
        }
    }

    let bytes = client.get_bytes(INDEX_URL).await?;
    let index: RuntimeIndex = serde_json::from_slice(&bytes).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("cannot parse the java runtime index: {err}"),
        )
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &bytes)?;

    Ok(index)
}

/// Installs the runtime for `component` into `jdk-<major>`, atomically.
pub async fn install(
    client: &HttpClient,
    root: &DataRoot,
    resolved: &Resolved,
    threads: usize,
    sink: &dyn EventSink,
) -> MlrcResult<JavaInstallation> {
    let manifest: RuntimeManifest = client.get_json(&resolved.manifest_url).await?;

    let staging = staging_dir(root, resolved.major);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    let mut tasks: BTreeMap<String, DownloadTask> = BTreeMap::new();
    for (path, file) in &manifest.files {
        if file.kind != "file" {
            continue;
        }
        let Some(raw) = file.downloads.as_ref().and_then(|d| d.raw.as_ref()) else {
            continue;
        };
        let dest = staging.join(Path::new(path));
        if tasks.contains_key(&raw.sha1) {
            if let Some(task) = tasks.get_mut(&raw.sha1) {
                task.dests.push(dest);
            }
            continue;
        }
        tasks.insert(
            raw.sha1.clone(),
            DownloadTask {
                sha1: raw.sha1.clone(),
                url: raw.url.clone(),
                size: raw.size,
                dests: vec![dest],
                required: true,
            },
        );
    }

    let store = ObjectStore::new(root);
    let downloader = Downloader {
        client,
        store: &store,
        sink,
        threads,
        attempts: 3,
    };
    let referenced: BTreeSet<String> = tasks.keys().cloned().collect();
    downloader.run(tasks.into_values().collect()).await?;

    apply_permissions(&manifest, &staging);
    write_metadata(root, resolved)?;
    crate::port::state::write_objects(&staging, &referenced)?;

    let final_dir = root.jdk_dir(resolved.major);
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&staging, &final_dir)?;

    discover::managed(root, resolved.major).ok_or_else(|| {
        MlrcError::new(
            ErrorCode::JavaMissing,
            format!(
                "java {} was installed but does not validate; check {}",
                resolved.major,
                final_dir.display()
            ),
        )
    })
}

pub fn staging_dir(root: &DataRoot, major: u32) -> PathBuf {
    root.java_dir().join(format!("jdk-{major}.part"))
}

#[cfg(unix)]
fn apply_permissions(manifest: &RuntimeManifest, staging: &Path) {
    use std::os::unix::fs::PermissionsExt;

    for (path, file) in &manifest.files {
        if !file.executable {
            continue;
        }
        let target = staging.join(Path::new(path));
        if let Ok(metadata) = std::fs::metadata(&target) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            let _ = std::fs::set_permissions(&target, permissions);
        }
    }
}

/// On Windows every file is executable in the practical sense; nothing to do.
#[cfg(not(unix))]
fn apply_permissions(_manifest: &RuntimeManifest, _staging: &Path) {}

fn write_metadata(root: &DataRoot, resolved: &Resolved) -> MlrcResult<()> {
    let dir = staging_dir(root, resolved.major);
    std::fs::create_dir_all(&dir)?;
    let metadata = serde_json::json!({
        "component": resolved.component,
        "major": resolved.major,
        "version": resolved.version,
        "manifest": resolved.manifest_url,
    });
    let text = serde_json::to_string_pretty(&metadata)
        .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))?;
    std::fs::write(dir.join("mlrc.json"), text)?;
    Ok(())
}

fn is_fresh(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age < MAX_AGE)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> RuntimeIndex {
        let json = r#"{
            "windows-x64": {
                "java-runtime-beta": [{"manifest":{"url":"u-beta"},"version":{"name":"17.0.1"}}],
                "java-runtime-gamma": [{"manifest":{"url":"u-gamma"},"version":{"name":"17.0.15"}}],
                "java-runtime-gamma-snapshot": [{"manifest":{"url":"u-snap"},"version":{"name":"17.0.16"}}],
                "java-runtime-epsilon": [{"manifest":{"url":"u-eps"},"version":{"name":"25.0.1"}}]
            }
        }"#;
        serde_json::from_str(json).expect("index parses")
    }

    #[test]
    fn component_target_reports_its_major() {
        let resolved = resolve(
            &index(),
            "windows-x64",
            Target::Component("java-runtime-epsilon"),
        )
        .expect("component resolves");
        assert_eq!(resolved.major, 25);
        assert_eq!(resolved.manifest_url, "u-eps");
    }

    #[test]
    fn major_target_picks_the_newest_non_snapshot() {
        let resolved = resolve(&index(), "windows-x64", Target::Major(17)).expect("major resolves");
        assert_eq!(resolved.component, "java-runtime-gamma");
        assert_eq!(resolved.version, "17.0.15");
    }

    #[test]
    fn unknown_targets_do_not_resolve() {
        assert!(resolve(&index(), "windows-x64", Target::Major(8)).is_none());
        assert!(resolve(&index(), "linux", Target::Major(17)).is_none());
    }
}
