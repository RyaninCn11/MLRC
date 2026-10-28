//! Mods: which loader an installed version runs, and installing jars into `mods/`.

pub mod modrinth;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::{Event, EventSink};
use crate::fetch::HttpClient;
use crate::meta::version::VersionJson;
use crate::port::DataRoot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub mc: String,
    pub loader: String,
}

/// The loader an installed id belongs to, if any.
pub fn loader_of(id: &str) -> Option<&'static str> {
    if id.starts_with("fabric-loader") {
        Some("fabric")
    } else if id.starts_with("neoforge-") {
        Some("neoforge")
    } else if id.starts_with("forge-") || id.contains("-forge-") {
        Some("forge")
    } else {
        None
    }
}

/// Game version plus loader for an installed version.
///
/// The Minecraft version comes from `inheritsFrom`, not from the id: `neoforge-21.1.265`
/// does not contain it, and `1.21.1-forge-52.1.20` would need parsing.
pub fn platform(root: &DataRoot, id: &str) -> MlrcResult<Platform> {
    let loader = loader_of(id).ok_or_else(|| {
        MlrcError::new(
            ErrorCode::Usage,
            format!(
                "{id} is a vanilla version; mods need a loader (install with --loader fabric|forge|neoforge)"
            ),
        )
        .with("version", id.to_string())
    })?;

    let document = VersionJson::read_cached(root, id)?.ok_or_else(|| {
        MlrcError::new(
            ErrorCode::VersionUnknown,
            format!("metadata for {id} is missing; reinstall it"),
        )
    })?;

    let mc = document
        .inherits_from
        .clone()
        .unwrap_or_else(|| id.to_string());

    Ok(Platform {
        mc,
        loader: loader.to_string(),
    })
}

pub fn mods_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("mods")
}

#[derive(Debug, Clone)]
pub struct InstalledMod {
    pub name: String,
    pub filename: String,
    pub bytes: u64,
    /// The file was already in `mods/`, so nothing was downloaded.
    pub already_present: bool,
}

enum Target {
    Project(String),
    Version(String),
}

pub async fn install(
    http: &HttpClient,
    game_dir: &Path,
    platform: &Platform,
    project: &str,
    sink: &dyn EventSink,
) -> MlrcResult<Vec<InstalledMod>> {
    let mut seen = BTreeSet::new();
    let mut versions = Vec::new();
    collect(
        http,
        Target::Project(project.to_string()),
        platform,
        0,
        &mut seen,
        &mut versions,
    )
    .await?;

    let dir = mods_dir(game_dir);
    std::fs::create_dir_all(&dir)?;

    let mut installed = Vec::new();
    for version in versions {
        let file = version.primary_file()?.clone();
        let target = dir.join(&file.filename);

        if target.is_file() {
            installed.push(InstalledMod {
                name: version.name.clone(),
                filename: file.filename.clone(),
                bytes: 0,
                already_present: true,
            });
            continue;
        }

        sink.emit(Event::FileStarted {
            path: file.filename.clone(),
            total_bytes: None,
        });

        let bytes = http.get_bytes(&file.url).await?;
        if !file.hashes.sha1.is_empty() {
            let actual = sha1_of(&bytes);
            if actual != file.hashes.sha1 {
                return Err(MlrcError::new(
                    ErrorCode::HashMismatch,
                    format!("hash mismatch for {}", file.filename),
                )
                .with("expected", file.hashes.sha1.clone())
                .with("actual", actual));
            }
        }

        // Write beside the target first: a half-written jar in `mods/` breaks the game.
        let part = target.with_extension("jar.part");
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &target)?;

        sink.emit(Event::FileFinished {
            path: file.filename.clone(),
            ok: true,
            cached: false,
        });

        installed.push(InstalledMod {
            name: version.name.clone(),
            filename: file.filename.clone(),
            bytes: bytes.len() as u64,
            already_present: false,
        });
    }

    Ok(installed)
}

/// Depth-first over required dependencies, de-duplicated by project.
async fn collect(
    http: &HttpClient,
    target: Target,
    platform: &Platform,
    depth: usize,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<modrinth::Version>,
) -> MlrcResult<()> {
    if depth > 8 {
        return Ok(());
    }

    let version = match target {
        Target::Project(project) => {
            modrinth::version_for(http, &project, &platform.mc, &platform.loader).await?
        }
        Target::Version(id) => modrinth::version_by_id(http, &id).await?,
    };

    if !seen.insert(version.project_id.clone()) {
        return Ok(());
    }

    let dependencies: Vec<Target> = version
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == "required")
        .filter_map(|dependency| {
            if let Some(id) = &dependency.version_id {
                Some(Target::Version(id.clone()))
            } else {
                dependency.project_id.clone().map(Target::Project)
            }
        })
        .collect();

    out.push(version);

    for dependency in dependencies {
        Box::pin(collect(http, dependency, platform, depth + 1, seen, out)).await?;
    }

    Ok(())
}

pub fn sha1_of(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaders_are_recognised_from_ids() {
        assert_eq!(loader_of("fabric-loader-0.20.1-1.21.1"), Some("fabric"));
        assert_eq!(loader_of("1.21.1-forge-52.1.20"), Some("forge"));
        assert_eq!(loader_of("neoforge-21.1.265"), Some("neoforge"));
        assert_eq!(loader_of("1.21.1"), None);
        assert_eq!(loader_of("26.3"), None);
    }
}
