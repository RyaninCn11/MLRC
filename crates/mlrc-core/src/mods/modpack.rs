//! `modrinth.index.json` packs (`.mrpack`).
//!
//! A pack is a zip holding an index of files to download plus an `overrides/` tree.
//! Importing one means: install the loader the pack asks for, create the instance,
//! unpack the overrides, then fetch the listed files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::{Event, EventSink};
use crate::fetch::HttpClient;
use crate::install::{install, install_installer, install_loader, InstallOptions};
use crate::instance::Instance;
use crate::loader::forge::{self, Flavour};
use crate::mods::curseforge;
use crate::mods::manifest::{Entry, Manifest};
use crate::port::DataRoot;

pub const INDEX_NAME: &str = "modrinth.index.json";
pub const CURSEFORGE_MANIFEST: &str = "manifest.json";

#[derive(Debug, Clone, Deserialize)]
struct Index {
    #[serde(default)]
    name: String,
    #[serde(default)]
    files: Vec<PackFile>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PackFile {
    path: String,
    #[serde(default)]
    hashes: Hashes,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    env: Option<FileEnv>,
    #[serde(default, rename = "fileSize")]
    file_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Hashes {
    #[serde(default)]
    sha1: String,
}

#[derive(Debug, Clone, Deserialize)]
struct FileEnv {
    #[serde(default)]
    client: String,
}

#[derive(Debug, Clone)]
pub struct ImportSummary {
    pub instance: String,
    pub version: String,
    pub minecraft: String,
    pub loader: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub skipped: usize,
}

/// What an import would do, without writing anything.
#[derive(Debug, Clone)]
pub struct PreviewFile {
    /// A path for Modrinth packs, `projectID/fileID` for CurseForge ones.
    pub label: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct PackPreview {
    pub kind: Kind,
    pub name: String,
    pub minecraft: String,
    pub loader: Option<(String, String)>,
    pub files: Vec<PreviewFile>,
    /// Files the pack marks optional, which an import skips.
    pub skipped: usize,
    pub overrides: usize,
}

pub fn preview(pack_path: &Path) -> MlrcResult<PackPreview> {
    match detect(pack_path)? {
        Kind::Modrinth => {
            let pack = Pack::open(pack_path)?;
            let files: Vec<PreviewFile> = pack
                .index
                .files
                .iter()
                .filter(|file| !client_unsupported(file))
                .map(|file| PreviewFile {
                    label: file.path.clone(),
                    bytes: file.file_size,
                })
                .collect();

            Ok(PackPreview {
                skipped: pack.index.files.len() - files.len(),
                kind: Kind::Modrinth,
                name: pack.name().to_string(),
                minecraft: pack.minecraft()?,
                loader: pack
                    .loader()
                    .map(|(name, version)| (name.to_string(), version)),
                files,
                overrides: pack.overrides.len(),
            })
        }
        Kind::CurseForge => {
            let pack = CfPack::open(pack_path)?;
            // Names would cost one request per file, so a preview shows the ids the
            // manifest already has.
            let files: Vec<PreviewFile> = pack
                .manifest
                .files
                .iter()
                .filter(|file| file.required)
                .map(|file| PreviewFile {
                    label: format!("{}/{}", file.project_id, file.file_id),
                    bytes: 0,
                })
                .collect();

            Ok(PackPreview {
                skipped: pack.manifest.files.len() - files.len(),
                kind: Kind::CurseForge,
                name: pack.name().to_string(),
                minecraft: pack.minecraft()?,
                loader: pack
                    .loader()
                    .map(|(name, version)| (name.to_string(), version)),
                files,
                overrides: pack.overrides.len(),
            })
        }
    }
}

fn client_unsupported(file: &PackFile) -> bool {
    file.env
        .as_ref()
        .map(|env| env.client == "unsupported")
        .unwrap_or(false)
}

/// Reads the pack's index and overrides without touching the data directory.
pub struct Pack {
    index: Index,
    overrides: Vec<(PathBuf, Vec<u8>)>,
}

impl Pack {
    pub fn open(path: &Path) -> MlrcResult<Self> {
        let file = std::fs::File::open(path).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot open {}: {err}", path.display()),
            )
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("{} is not a modpack archive: {err}", path.display()),
            )
        })?;

        let mut index_text = String::new();
        {
            use std::io::Read;
            let mut entry = archive.by_name(INDEX_NAME).map_err(|_| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    format!("{} has no {INDEX_NAME}", path.display()),
                )
            })?;
            entry.read_to_string(&mut index_text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    format!("cannot read {INDEX_NAME}: {err}"),
                )
            })?;
        }

        let index: Index = serde_json::from_str(&index_text).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse {INDEX_NAME}: {err}"),
            )
        })?;

        let mut overrides = Vec::new();
        for position in 0..archive.len() {
            let mut entry = match archive.by_index(position) {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let Some(relative) = name
                .strip_prefix("overrides/")
                .or_else(|| name.strip_prefix("client-overrides/"))
            else {
                continue;
            };
            let Some(relative) = safe_relative(relative) else {
                continue;
            };

            use std::io::Read;
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            overrides.push((relative, bytes));
        }

        Ok(Self { index, overrides })
    }

    pub fn name(&self) -> &str {
        &self.index.name
    }

    pub fn minecraft(&self) -> MlrcResult<String> {
        self.index
            .dependencies
            .get("minecraft")
            .cloned()
            .ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    "the pack does not name a minecraft version",
                )
            })
    }

    /// `(loader name, loader version)` for anything the pack pins.
    pub fn loader(&self) -> Option<(&'static str, String)> {
        if let Some(version) = self.index.dependencies.get("fabric-loader") {
            return Some(("fabric", version.clone()));
        }
        if let Some(version) = self.index.dependencies.get("neoforge") {
            return Some(("neoforge", version.clone()));
        }
        if let Some(version) = self.index.dependencies.get("forge") {
            return Some(("forge", version.clone()));
        }
        None
    }
}

/// Which flavour of pack the archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `modrinth.index.json`, files given as URLs.
    Modrinth,
    /// `manifest.json`, files given as project and file ids that need the API.
    CurseForge,
}

pub fn detect(path: &Path) -> MlrcResult<Kind> {
    let file = std::fs::File::open(path).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot open {}: {err}", path.display()),
        )
    })?;
    let archive = zip::ZipArchive::new(file).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("{} is not a modpack archive: {err}", path.display()),
        )
    })?;

    let names: Vec<&str> = archive.file_names().collect();
    if names.contains(&INDEX_NAME) {
        Ok(Kind::Modrinth)
    } else if names.contains(&CURSEFORGE_MANIFEST) {
        Ok(Kind::CurseForge)
    } else {
        Err(MlrcError::new(
            ErrorCode::MetaShape,
            format!(
                "{} is neither a modrinth nor a curseforge pack (no {INDEX_NAME} or {CURSEFORGE_MANIFEST})",
                path.display()
            ),
        ))
    }
}

pub async fn import(
    client: &HttpClient,
    root: &DataRoot,
    pack_path: &Path,
    instance_name: Option<&str>,
    key: Option<&str>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<ImportSummary> {
    match detect(pack_path)? {
        Kind::Modrinth => {
            import_modrinth(client, root, pack_path, instance_name, options, sink).await
        }
        Kind::CurseForge => {
            import_curseforge(client, root, pack_path, instance_name, key, options, sink).await
        }
    }
}

async fn import_modrinth(
    client: &HttpClient,
    root: &DataRoot,
    pack_path: &Path,
    instance_name: Option<&str>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<ImportSummary> {
    let pack = Pack::open(pack_path)?;
    let minecraft = pack.minecraft()?;

    let name = match instance_name {
        Some(name) => name.to_string(),
        None => {
            // The pack's own name is what the user recognises; the file name is the
            // fallback for packs that leave it out.
            let from_index = pack.name().trim();
            let stem = pack_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "pack".to_string());
            sanitize(if from_index.is_empty() {
                &stem
            } else {
                from_index
            })
        }
    };
    Instance::validate_name(&name)?;

    if Instance::load(root, &name)?.is_some() {
        return Err(
            MlrcError::new(ErrorCode::Usage, format!("instance {name} already exists"))
                .with("instance", name),
        );
    }

    // The loader the pack was built against, installed exactly.
    let (version_id, loader_name) =
        ensure_version(client, root, &minecraft, pack.loader(), options, sink).await?;

    let instance = Instance::create(
        root,
        &name,
        &version_id,
        crate::port::config::ForkConfig::default(),
    )?;
    let game_dir = Instance::game_dir(root, &name);

    // Overrides first: a pack's config files must be in place before the first launch.
    for (relative, bytes) in &pack.overrides {
        let target = game_dir.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }

    let mut manifest = Manifest::load(&game_dir)?;
    manifest.minecraft = minecraft.clone();
    manifest.loader = loader_name.clone().unwrap_or_default();

    let mut files = 0usize;
    let mut bytes_total = 0u64;
    let mut skipped = 0usize;

    for file in &pack.index.files {
        let Some(relative) = safe_relative(&file.path) else {
            skipped += 1;
            continue;
        };
        if file
            .env
            .as_ref()
            .map(|env| env.client == "unsupported")
            .unwrap_or(false)
        {
            skipped += 1;
            continue;
        }
        let Some(url) = file.downloads.first() else {
            skipped += 1;
            continue;
        };

        let target = game_dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        sink.emit(Event::FileStarted {
            path: relative.display().to_string(),
            total_bytes: None,
        });

        let bytes = crate::mods::fetch_bytes(client, url).await?;
        if !file.hashes.sha1.is_empty() {
            let actual = crate::mods::sha1_of(&bytes);
            if actual != file.hashes.sha1 {
                return Err(MlrcError::new(
                    ErrorCode::HashMismatch,
                    format!("hash mismatch for {}", relative.display()),
                )
                .with("expected", file.hashes.sha1.clone())
                .with("actual", actual));
            }
        }

        let part = target.with_extension("part");
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &target)?;

        sink.emit(Event::FileFinished {
            path: relative.display().to_string(),
            ok: true,
        });

        files += 1;
        bytes_total += bytes.len() as u64;

        // Everything under mods/ is a mod whose project the pack did not name.
        if relative.starts_with("mods") {
            manifest.upsert(Entry {
                project: target
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default(),
                project_id: String::new(),
                name: String::new(),
                version_id: String::new(),
                version_number: String::new(),
                filename: target
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                sha1: file.hashes.sha1.clone(),
                source: String::new(),
                kind: String::new(),
            });
        }
    }

    manifest.save(&game_dir)?;

    Ok(ImportSummary {
        instance: instance.name,
        version: version_id,
        minecraft,
        loader: loader_name,
        files,
        bytes: bytes_total,
        skipped,
    })
}

/// A CurseForge pack: `manifest.json` plus an overrides tree.
pub struct CfPack {
    manifest: CfManifest,
    overrides: Vec<(PathBuf, Vec<u8>)>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CfManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    minecraft: CfMinecraft,
    #[serde(default)]
    files: Vec<CfFileRef>,
    #[serde(default)]
    overrides: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CfMinecraft {
    #[serde(default)]
    version: String,
    #[serde(default, rename = "modLoaders")]
    mod_loaders: Vec<CfLoader>,
}

#[derive(Debug, Clone, Deserialize)]
struct CfLoader {
    #[serde(default)]
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CfFileRef {
    #[serde(default, rename = "projectID")]
    project_id: u64,
    #[serde(default, rename = "fileID")]
    file_id: u64,
    #[serde(default = "default_true")]
    required: bool,
}

fn default_true() -> bool {
    true
}

impl CfPack {
    pub fn open(path: &Path) -> MlrcResult<Self> {
        let file = std::fs::File::open(path).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot open {}: {err}", path.display()),
            )
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("{} is not a modpack archive: {err}", path.display()),
            )
        })?;

        let mut text = String::new();
        {
            use std::io::Read;
            let mut entry = archive.by_name(CURSEFORGE_MANIFEST).map_err(|_| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    format!("{} has no {CURSEFORGE_MANIFEST}", path.display()),
                )
            })?;
            entry.read_to_string(&mut text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    format!("cannot read {CURSEFORGE_MANIFEST}: {err}"),
                )
            })?;
        }

        let manifest: CfManifest = serde_json::from_str(&text).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse {CURSEFORGE_MANIFEST}: {err}"),
            )
        })?;

        // The manifest names the overrides folder; `overrides` is the default.
        let folder = if manifest.overrides.trim().is_empty() {
            "overrides".to_string()
        } else {
            manifest.overrides.trim().to_string()
        };
        let prefix = format!("{folder}/");

        let mut overrides = Vec::new();
        for position in 0..archive.len() {
            let mut entry = match archive.by_index(position) {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let Some(relative) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(relative) = safe_relative(relative) else {
                continue;
            };

            use std::io::Read;
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_err() {
                continue;
            }
            overrides.push((relative, bytes));
        }

        Ok(Self {
            manifest,
            overrides,
        })
    }

    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn minecraft(&self) -> MlrcResult<String> {
        let version = self.manifest.minecraft.version.trim();
        if version.is_empty() {
            return Err(MlrcError::new(
                ErrorCode::MetaShape,
                "the pack does not name a minecraft version",
            ));
        }
        Ok(version.to_string())
    }

    /// The primary loader, e.g. `forge-47.2.0` becomes `("forge", "47.2.0")`.
    pub fn loader(&self) -> Option<(&'static str, String)> {
        let chosen = self
            .manifest
            .minecraft
            .mod_loaders
            .iter()
            .find(|loader| loader.primary)
            .or_else(|| self.manifest.minecraft.mod_loaders.first())?;

        let id = chosen.id.trim();
        for (prefix, name) in [
            ("neoforge-", "neoforge"),
            ("quilt-", "quilt"),
            ("fabric-", "fabric"),
            ("forge-", "forge"),
        ] {
            if let Some(rest) = id.strip_prefix(prefix) {
                return Some((name, rest.to_string()));
            }
        }
        None
    }
}

async fn import_curseforge(
    client: &HttpClient,
    root: &DataRoot,
    pack_path: &Path,
    instance_name: Option<&str>,
    key: Option<&str>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<ImportSummary> {
    let Some(key) = key else {
        return Err(MlrcError::new(
            ErrorCode::Config,
            "curseforge packs download by project id and need an api key; \
             run `mlrc config set curseforge.api_key <key>`",
        ));
    };

    let pack = CfPack::open(pack_path)?;
    let minecraft = pack.minecraft()?;

    let name = match instance_name {
        Some(name) => name.to_string(),
        None => {
            let from_manifest = pack.name().trim();
            let stem = pack_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "pack".to_string());
            sanitize(if from_manifest.is_empty() {
                &stem
            } else {
                from_manifest
            })
        }
    };
    Instance::validate_name(&name)?;

    if Instance::load(root, &name)?.is_some() {
        return Err(
            MlrcError::new(ErrorCode::Usage, format!("instance {name} already exists"))
                .with("instance", name),
        );
    }

    let (version_id, loader_name) =
        ensure_version(client, root, &minecraft, pack.loader(), options, sink).await?;

    let instance = Instance::create(
        root,
        &name,
        &version_id,
        crate::port::config::ForkConfig::default(),
    )?;
    let game_dir = Instance::game_dir(root, &name);

    for (relative, bytes) in &pack.overrides {
        let target = game_dir.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }

    let mods_dir = crate::mods::mods_dir(&game_dir);
    std::fs::create_dir_all(&mods_dir)?;

    let mut manifest = Manifest::load(&game_dir)?;
    manifest.minecraft = minecraft.clone();
    manifest.loader = loader_name.clone().unwrap_or_default();

    let mut files = 0usize;
    let mut bytes_total = 0u64;
    let mut skipped = 0usize;

    for file in &pack.manifest.files {
        // Optional files are opt-in; install only what the pack requires.
        if !file.required {
            skipped += 1;
            continue;
        }

        let resolved = curseforge::file(client, key, file.project_id, file.file_id).await?;
        let target = mods_dir.join(&resolved.file_name);

        if !target.is_file() {
            sink.emit(Event::FileStarted {
                path: resolved.file_name.clone(),
                total_bytes: Some(resolved.size).filter(|size| *size > 0),
            });

            let bytes = crate::mods::fetch_bytes(client, &resolved.download_url).await?;
            if !resolved.sha1.is_empty() {
                let actual = crate::mods::sha1_of(&bytes);
                if actual != resolved.sha1 {
                    return Err(MlrcError::new(
                        ErrorCode::HashMismatch,
                        format!("hash mismatch for {}", resolved.file_name),
                    )
                    .with("expected", resolved.sha1.clone())
                    .with("actual", actual));
                }
            }

            let part = target.with_extension("jar.part");
            std::fs::write(&part, &bytes)?;
            std::fs::rename(&part, &target)?;

            sink.emit(Event::FileFinished {
                path: resolved.file_name.clone(),
                ok: true,
            });

            bytes_total += bytes.len() as u64;
        }

        files += 1;
        manifest.upsert(Entry {
            project: resolved.mod_id.to_string(),
            project_id: resolved.mod_id.to_string(),
            name: resolved.display_name.clone(),
            version_id: resolved.file_id.to_string(),
            version_number: resolved.display_name.clone(),
            filename: resolved.file_name.clone(),
            sha1: resolved.sha1.clone(),
            source: "curseforge".to_string(),
            kind: String::new(),
        });
    }

    manifest.save(&game_dir)?;

    Ok(ImportSummary {
        instance: instance.name,
        version: version_id,
        minecraft,
        loader: loader_name,
        files,
        bytes: bytes_total,
        skipped,
    })
}

/// Installs the loader a pack pins (or the vanilla version) and returns its id.
async fn ensure_version(
    client: &HttpClient,
    root: &DataRoot,
    minecraft: &str,
    loader: Option<(&str, String)>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<(String, Option<String>)> {
    match loader {
        Some((family_name, loader_version))
            if crate::loader::fabric::Family::parse(family_name).is_some() =>
        {
            let family =
                crate::loader::fabric::Family::parse(family_name).expect("checked by the guard");
            let state = install_loader(
                client,
                root,
                minecraft,
                family,
                Some(&loader_version),
                options,
                sink,
            )
            .await?;
            Ok((state.id, Some(family_name.to_string())))
        }
        Some((flavour_name, loader_version)) => {
            let flavour = Flavour::parse(flavour_name).ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::LoaderUnavailable,
                    format!("the pack asks for {flavour_name}, which is not supported"),
                )
            })?;
            let installer =
                forge::resolve(client, flavour, minecraft, Some(&loader_version)).await?;
            let state =
                install_installer(client, root, minecraft, &installer, options, sink).await?;
            Ok((state.id, Some(flavour_name.to_string())))
        }
        None => {
            let state = install(client, root, minecraft, options, sink).await?;
            Ok((state.id, None))
        }
    }
}

/// Rejects absolute paths and `..`, so an archive cannot write outside the game dir.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let normalized = path.replace('\\', "/");
    let candidate = PathBuf::from(&normalized);

    // `is_absolute` only knows the current platform: on Unix `C:/x` is an ordinary
    // relative path, so a pack authored on Windows would land in a directory named
    // `C:` there. Reject the drive designator everywhere and a pack behaves the same
    // on both.
    if candidate.is_absolute() || starts_with_drive(&normalized) {
        return None;
    }

    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => clean.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }

    (!clean.as_os_str().is_empty()).then_some(clean)
}

fn starts_with_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "pack".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_rejected() {
        assert!(safe_relative("mods/../evil.jar").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert!(safe_relative("..\\evil").is_none());
        // A drive designator is absolute on Windows and merely odd on Unix, so it is
        // rejected on both: the same archive must behave the same everywhere.
        assert!(safe_relative("C:/windows/system32").is_none());
        assert!(safe_relative("C:\\windows\\system32").is_none());
        assert!(safe_relative("c:relative-on-windows").is_none());
    }

    #[test]
    fn ordinary_paths_survive() {
        assert_eq!(
            safe_relative("mods/sodium.jar"),
            Some(PathBuf::from("mods/sodium.jar"))
        );
        assert_eq!(
            safe_relative("config/foo/bar.toml"),
            Some(PathBuf::from("config/foo/bar.toml"))
        );
    }

    #[test]
    fn instance_names_are_made_safe() {
        assert_eq!(
            sanitize("Fabulously Optimized 6.1"),
            "Fabulously-Optimized-6-1"
        );
        assert_eq!(sanitize("***"), "pack");
    }
}
