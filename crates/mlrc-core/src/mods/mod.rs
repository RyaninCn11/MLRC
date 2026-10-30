//! Mods: which loader an installed version runs, and installing jars into `mods/`.

pub mod curseforge;
pub mod manifest;
pub mod modpack;
pub mod modrinth;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::{Event, EventSink};
use crate::fetch::HttpClient;
use crate::meta::version::VersionJson;
use crate::port::DataRoot;

use manifest::{Entry, Manifest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub mc: String,
    pub loader: String,
}

/// The loader an installed id belongs to, if any.
pub fn loader_of(id: &str) -> Option<&'static str> {
    if id.starts_with("fabric-loader") {
        Some("fabric")
    } else if id.starts_with("quilt-loader") {
        Some("quilt")
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
/// Follows `inheritsFrom` rather than parsing the id: a fork's id says nothing about
/// its loader, and `neoforge-21.1.265` does not contain the Minecraft version at all.
pub fn platform(root: &DataRoot, id: &str) -> MlrcResult<Platform> {
    let mut current = id.to_string();

    for _ in 0..8 {
        let document = VersionJson::read_cached(root, &current)?.ok_or_else(|| {
            MlrcError::new(
                ErrorCode::VersionUnknown,
                format!("metadata for {current} is missing; reinstall it"),
            )
        })?;

        if let Some(loader) = loader_of(&current) {
            let mc = document
                .inherits_from
                .clone()
                .unwrap_or_else(|| current.clone());
            return Ok(Platform {
                mc,
                loader: loader.to_string(),
            });
        }

        match document.inherits_from.clone() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    Err(MlrcError::new(
        ErrorCode::Usage,
        format!(
            "{id} is a vanilla version; mods need a loader (install with --loader fabric|quilt|forge|neoforge)"
        ),
    )
    .with("version", id.to_string()))
}

pub fn mods_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("mods")
}

/// What `mod install --dry-run` prints: resolved, not downloaded.
#[derive(Debug, Clone)]
pub struct PlannedMod {
    pub name: String,
    pub version: String,
    pub filename: String,
    pub size: u64,
}

/// Resolves a project and its required dependencies without writing anything.
pub async fn plan(
    http: &HttpClient,
    platform: &Platform,
    project: &str,
    source: Source,
    key: Option<&str>,
) -> MlrcResult<Vec<PlannedMod>> {
    match source {
        Source::Modrinth => {
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

            versions
                .iter()
                .map(|version| {
                    let file = version.primary_file()?;
                    Ok(PlannedMod {
                        name: version.name.clone(),
                        version: version.version_number.clone(),
                        filename: file.filename.clone(),
                        size: file.size,
                    })
                })
                .collect()
        }
        Source::CurseForge => {
            let key = key.ok_or_else(missing_key)?;
            let mut seen = BTreeSet::new();
            let mut files = Vec::new();
            collect_curseforge(
                http,
                key,
                project.to_string(),
                platform,
                0,
                &mut seen,
                &mut files,
            )
            .await?;

            Ok(files
                .into_iter()
                .map(|file| PlannedMod {
                    name: file.display_name.clone(),
                    version: file.display_name,
                    filename: file.file_name,
                    size: file.size,
                })
                .collect())
        }
    }
}

/// Downloads with a few attempts. A single timeout on a CDN should not fail an
/// otherwise healthy install, and a retry is cheaper than asking the user to repeat
/// the command.
pub async fn fetch_bytes(http: &HttpClient, url: &str) -> MlrcResult<Vec<u8>> {
    let mut last: Option<MlrcError> = None;

    for attempt in 0..3u64 {
        match http.get_bytes(url).await {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                last = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(400 * (attempt + 1))).await;
            }
        }
    }

    Err(last.expect("the loop always runs at least once"))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Modrinth,
    CurseForge,
}

impl Source {
    pub fn parse(value: &str) -> MlrcResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "modrinth" | "" => Ok(Self::Modrinth),
            "curseforge" | "cf" => Ok(Self::CurseForge),
            other => Err(MlrcError::new(
                ErrorCode::Usage,
                format!("unknown mod source {other} (expected modrinth or curseforge)"),
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Modrinth => "modrinth",
            Self::CurseForge => "curseforge",
        }
    }
}

/// Installs from whichever source the caller picked.
///
/// `key` is only needed for CurseForge, which authenticates every request.
pub async fn install_from(
    http: &HttpClient,
    game_dir: &Path,
    platform: &Platform,
    project: &str,
    source: Source,
    key: Option<&str>,
    sink: &dyn EventSink,
) -> MlrcResult<Vec<InstalledMod>> {
    match source {
        Source::Modrinth => install(http, game_dir, platform, project, sink).await,
        Source::CurseForge => {
            let key = key.ok_or_else(missing_key)?;
            install_curseforge(http, game_dir, platform, key, project, sink).await
        }
    }
}

fn missing_key() -> MlrcError {
    MlrcError::new(
        ErrorCode::Config,
        "curseforge needs an api key; run `mlrc config set curseforge.api_key <key>` \
         or set MLRC_CURSEFORGE_KEY",
    )
}

async fn install_curseforge(
    http: &HttpClient,
    game_dir: &Path,
    platform: &Platform,
    key: &str,
    project: &str,
    sink: &dyn EventSink,
) -> MlrcResult<Vec<InstalledMod>> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    collect_curseforge(
        http,
        key,
        project.to_string(),
        platform,
        0,
        &mut seen,
        &mut files,
    )
    .await?;

    let dir = mods_dir(game_dir);
    std::fs::create_dir_all(&dir)?;

    let mut manifest = Manifest::load(game_dir)?;
    manifest.minecraft = platform.mc.clone();
    manifest.loader = platform.loader.clone();

    let mut installed = Vec::new();
    for file in files {
        let target = dir.join(&file.file_name);
        let mut bytes = None;

        if target.is_file() {
            installed.push(InstalledMod {
                name: file.display_name.clone(),
                filename: file.file_name.clone(),
                bytes: 0,
                already_present: true,
            });
        } else {
            sink.emit(Event::FileStarted {
                path: file.file_name.clone(),
                total_bytes: Some(file.size).filter(|size| *size > 0),
            });

            let data = fetch_bytes(http, &file.download_url).await?;
            if !file.sha1.is_empty() {
                let actual = sha1_of(&data);
                if actual != file.sha1 {
                    return Err(MlrcError::new(
                        ErrorCode::HashMismatch,
                        format!("hash mismatch for {}", file.file_name),
                    )
                    .with("expected", file.sha1.clone())
                    .with("actual", actual));
                }
            }

            let part = target.with_extension("jar.part");
            std::fs::write(&part, &data)?;
            std::fs::rename(&part, &target)?;

            sink.emit(Event::FileFinished {
                path: file.file_name.clone(),
                ok: true,
                cached: false,
            });

            installed.push(InstalledMod {
                name: file.display_name.clone(),
                filename: file.file_name.clone(),
                bytes: data.len() as u64,
                already_present: false,
            });
            bytes = Some(data.len() as u64);
        }
        let _ = bytes;

        manifest.upsert(Entry {
            project: file.mod_id.to_string(),
            project_id: file.mod_id.to_string(),
            name: file.display_name.clone(),
            version_id: file.file_id.to_string(),
            version_number: file.display_name.clone(),
            filename: file.file_name.clone(),
            sha1: file.sha1.clone(),
            source: Source::CurseForge.as_str().to_string(),
        });
    }

    manifest.save(game_dir)?;
    Ok(installed)
}

async fn collect_curseforge(
    http: &HttpClient,
    key: &str,
    project: String,
    platform: &Platform,
    depth: usize,
    seen: &mut BTreeSet<u64>,
    out: &mut Vec<curseforge::CfFile>,
) -> MlrcResult<()> {
    if depth > 8 {
        return Ok(());
    }

    let file = curseforge::resolve(http, key, &project, &platform.mc, &platform.loader).await?;
    if !seen.insert(file.mod_id) {
        return Ok(());
    }

    let required = file.required.clone();
    out.push(file);

    for dependency in required {
        Box::pin(collect_curseforge(
            http,
            key,
            dependency.to_string(),
            platform,
            depth + 1,
            seen,
            out,
        ))
        .await?;
    }

    Ok(())
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

    let mut manifest = Manifest::load(game_dir)?;
    manifest.minecraft = platform.mc.clone();
    manifest.loader = platform.loader.clone();

    let root_id = versions
        .first()
        .map(|version| version.project_id.clone())
        .unwrap_or_default();

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
            record(
                http,
                &mut manifest,
                project,
                &root_id,
                &version,
                &file.filename,
            )
            .await;
            continue;
        }

        sink.emit(Event::FileStarted {
            path: file.filename.clone(),
            total_bytes: None,
        });

        let bytes = fetch_bytes(http, &file.url).await?;
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
        record(
            http,
            &mut manifest,
            project,
            &root_id,
            &version,
            &file.filename,
        )
        .await;
    }

    manifest.save(game_dir)?;

    Ok(installed)
}

/// Remembers where a jar came from, so `mod update` and `mod remove` can find it.
///
/// The project the user asked for keeps its own name; a dependency is labelled with
/// its slug, fetched once, because its version document only carries an id.
async fn record(
    http: &HttpClient,
    manifest: &mut Manifest,
    requested: &str,
    root_id: &str,
    version: &modrinth::Version,
    filename: &str,
) {
    let project = if version.project_id == root_id {
        requested.to_string()
    } else if let Some(existing) = manifest
        .mods
        .iter()
        .find(|entry| entry.project_id == version.project_id)
    {
        existing.project.clone()
    } else {
        modrinth::project_slug(http, &version.project_id)
            .await
            .unwrap_or_else(|_| version.project_id.clone())
    };

    manifest.upsert(Entry {
        project,
        project_id: version.project_id.clone(),
        name: version.name.clone(),
        version_id: version.id.clone(),
        version_number: version.version_number.clone(),
        filename: filename.to_string(),
        sha1: String::new(),
        source: Source::Modrinth.as_str().to_string(),
    });
}

#[derive(Debug, Clone)]
pub struct UpdateOutcome {
    pub project: String,
    pub name: String,
    pub from_version: String,
    /// `None` means the installed version is already the newest one.
    pub to_version: Option<String>,
    pub filename: Option<String>,
    pub bytes: u64,
}

/// Result of an update pass.
pub struct UpdateReport {
    pub outcomes: Vec<UpdateOutcome>,
    /// Files that came from a modpack, whose project the pack never named.
    pub external: usize,
}

/// Re-resolves every recorded mod against its project and replaces outdated files.
pub async fn update(
    http: &HttpClient,
    game_dir: &Path,
    check_only: bool,
    key: Option<&str>,
    sink: &dyn EventSink,
) -> MlrcResult<UpdateReport> {
    let mut manifest = Manifest::load(game_dir)?;
    if manifest.mods.is_empty() {
        return Ok(UpdateReport {
            outcomes: Vec::new(),
            external: 0,
        });
    }

    let dir = mods_dir(game_dir);
    std::fs::create_dir_all(&dir)?;

    let mut outcomes = Vec::new();
    let mut external = 0usize;
    for entry in manifest.mods.clone() {
        // A pack's files carry no project id; only the pack can update those.
        if entry.project_id.is_empty() {
            external += 1;
            continue;
        }

        let source = Source::parse(&entry.source)?;
        let latest: Option<Latest> = match source {
            Source::Modrinth => {
                let version = modrinth::version_for(
                    http,
                    &entry.project_id,
                    &manifest.minecraft,
                    &manifest.loader,
                )
                .await?;
                if version.id == entry.version_id {
                    None
                } else {
                    let file = version.primary_file()?.clone();
                    Some(Latest {
                        id: version.id,
                        name: version.name,
                        version_number: version.version_number,
                        filename: file.filename,
                        url: file.url,
                        sha1: file.hashes.sha1,
                    })
                }
            }
            Source::CurseForge => {
                let key = key.ok_or_else(missing_key)?;
                let files = curseforge::files(
                    http,
                    key,
                    &entry.project_id,
                    &manifest.minecraft,
                    &manifest.loader,
                )
                .await?;

                // File ids grow with upload time, so "newer" is a comparison of ids.
                // Release type only decides which file to prefer, never whether one
                // is newer: otherwise a beta the pack pinned would look like an
                // update target in reverse.
                let installed: u64 = entry.version_id.parse().unwrap_or(0);
                let newest_release = files
                    .iter()
                    .filter(|file| file.release_type == 1)
                    .max_by_key(|file| file.file_id);
                let newest = files.iter().max_by_key(|file| file.file_id);

                let candidate = match newest_release {
                    Some(release) if release.file_id > installed => Some(release),
                    _ => newest.filter(|file| file.file_id > installed),
                };

                candidate.map(|file| Latest {
                    id: file.file_id.to_string(),
                    name: file.display_name.clone(),
                    version_number: file.display_name.clone(),
                    filename: file.file_name.clone(),
                    url: file.download_url.clone(),
                    sha1: file.sha1.clone(),
                })
            }
        };

        let Some(latest) = latest else {
            outcomes.push(UpdateOutcome {
                project: entry.project,
                name: entry.name,
                from_version: entry.version_number,
                to_version: None,
                filename: None,
                bytes: 0,
            });
            continue;
        };

        if check_only {
            outcomes.push(UpdateOutcome {
                project: entry.project,
                name: entry.name,
                from_version: entry.version_number,
                to_version: Some(latest.version_number.clone()),
                filename: Some(latest.filename.clone()),
                bytes: 0,
            });
            continue;
        }

        sink.emit(Event::FileStarted {
            path: latest.filename.clone(),
            total_bytes: None,
        });

        let bytes = fetch_bytes(http, &latest.url).await?;
        if !latest.sha1.is_empty() {
            let actual = sha1_of(&bytes);
            if actual != latest.sha1 {
                return Err(MlrcError::new(
                    ErrorCode::HashMismatch,
                    format!("hash mismatch for {}", latest.filename),
                )
                .with("expected", latest.sha1.clone())
                .with("actual", actual));
            }
        }

        // A disabled mod stays disabled across an update; the suffix is the state.
        let disabled = entry.filename.ends_with(".disabled");
        let new_name = if disabled {
            format!("{}.disabled", latest.filename)
        } else {
            latest.filename.clone()
        };

        // Replace in one step: write beside, then swap, then drop the old name.
        let target = dir.join(&new_name);
        let part = target.with_extension("jar.part");
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &target)?;

        let old = dir.join(&entry.filename);
        if old != target && old.is_file() {
            let _ = std::fs::remove_file(&old);
        }

        sink.emit(Event::FileFinished {
            path: new_name.clone(),
            ok: true,
            cached: false,
        });

        manifest.upsert(Entry {
            project: entry.project.clone(),
            project_id: entry.project_id.clone(),
            name: latest.name.clone(),
            version_id: latest.id.clone(),
            version_number: latest.version_number.clone(),
            filename: new_name.clone(),
            sha1: String::new(),
            source: entry.source.clone(),
        });

        outcomes.push(UpdateOutcome {
            project: entry.project,
            name: latest.name,
            from_version: entry.version_number,
            to_version: Some(latest.version_number),
            filename: Some(new_name),
            bytes: bytes.len() as u64,
        });
    }

    if !check_only {
        manifest.save(game_dir)?;
    }

    Ok(UpdateReport { outcomes, external })
}

#[derive(Debug, Clone)]
pub struct RemovedMod {
    pub project: String,
    pub filename: String,
}

/// The newest matching file, from whichever source recorded it.
struct Latest {
    id: String,
    name: String,
    version_number: String,
    filename: String,
    url: String,
    sha1: String,
}

pub fn remove(game_dir: &Path, needles: &[String]) -> MlrcResult<Vec<RemovedMod>> {
    let mut manifest = Manifest::load(game_dir)?;
    let dir = mods_dir(game_dir);
    let mut removed = Vec::new();

    for needle in needles {
        if let Some(entry) = manifest.find(needle).cloned() {
            let file = dir.join(&entry.filename);
            if file.is_file() {
                std::fs::remove_file(&file)?;
            }
            manifest.remove_project(&entry.project_id);
            removed.push(RemovedMod {
                project: entry.project,
                filename: entry.filename,
            });
            continue;
        }

        // A jar the manifest does not know about, dropped in by hand.
        let file = dir.join(needle);
        if file.is_file() {
            std::fs::remove_file(&file)?;
            removed.push(RemovedMod {
                project: needle.clone(),
                filename: needle.clone(),
            });
            continue;
        }

        return Err(MlrcError::new(
            ErrorCode::ModNotFound,
            format!("no installed mod matches {needle}"),
        )
        .with("name", needle.clone()));
    }

    manifest.save(game_dir)?;
    Ok(removed)
}

#[derive(Debug, Clone)]
pub struct ToggledMod {
    pub project: String,
    pub filename: String,
    pub enabled: bool,
    /// False when the file already had the requested state.
    pub changed: bool,
}

/// Enables or disables mods the way every other launcher does: renaming the jar.
///
/// Loaders only read `*.jar`, so `sodium.jar.disabled` is inert without deleting the
/// file or needing a second mods directory.
pub fn set_enabled(
    game_dir: &Path,
    needles: &[String],
    enabled: bool,
) -> MlrcResult<Vec<ToggledMod>> {
    let mut manifest = Manifest::load(game_dir)?;
    let dir = mods_dir(game_dir);
    let mut out = Vec::new();

    for needle in needles {
        let tracked = manifest.find(needle).cloned();
        let (project, current) = match &tracked {
            Some(entry) => (entry.project.clone(), entry.filename.clone()),
            None => (needle.clone(), needle.clone()),
        };

        let from = dir.join(&current);
        if !from.is_file() {
            return Err(MlrcError::new(
                ErrorCode::ModNotFound,
                format!("no installed mod matches {needle}"),
            )
            .with("name", needle.clone()));
        }

        let desired = if enabled {
            current.strip_suffix(".disabled").map(str::to_string)
        } else if current.ends_with(".jar") {
            Some(format!("{current}.disabled"))
        } else {
            None
        };

        let Some(desired) = desired else {
            out.push(ToggledMod {
                project,
                filename: current,
                enabled,
                changed: false,
            });
            continue;
        };

        let to = dir.join(&desired);
        if to.exists() {
            return Err(MlrcError::new(
                ErrorCode::ModConflict,
                format!("{desired} already exists"),
            )
            .with("file", desired.clone()));
        }

        std::fs::rename(&from, &to)?;

        if let Some(mut entry) = tracked {
            entry.filename = desired.clone();
            manifest.upsert(entry);
        }

        out.push(ToggledMod {
            project,
            filename: desired,
            enabled,
            changed: true,
        });
    }

    manifest.save(game_dir)?;
    Ok(out)
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
