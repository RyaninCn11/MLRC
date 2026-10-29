//! Which project every jar in `mods/` came from.
//!
//! Modrinth does not write anything into the file that identifies the project, so
//! updating later needs a record kept by the launcher. It lives beside the jars as a
//! dotfile, which loaders ignore.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

pub const FILE_NAME: &str = ".mlrc-mods.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub minecraft: String,
    pub loader: String,
    #[serde(default)]
    pub mods: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// What the user typed: a slug (`sodium`) or a project id.
    pub project: String,
    /// Always Modrinth's id, which is what a lookup can rely on.
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    pub version_id: String,
    #[serde(default)]
    pub version_number: String,
    pub filename: String,
    #[serde(default)]
    pub sha1: String,
}

impl Manifest {
    pub fn path(game_dir: &Path) -> PathBuf {
        crate::mods::mods_dir(game_dir).join(FILE_NAME)
    }

    pub fn load(game_dir: &Path) -> MlrcResult<Self> {
        let path = Self::path(game_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Config,
                    format!("invalid {}: {err}", path.display()),
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::new("", "")),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    pub fn new(minecraft: &str, loader: &str) -> Self {
        Self {
            schema: 1,
            minecraft: minecraft.to_string(),
            loader: loader.to_string(),
            mods: Vec::new(),
        }
    }

    pub fn save(&self, game_dir: &Path) -> MlrcResult<()> {
        let path = Self::path(game_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|err| {
            MlrcError::new(ErrorCode::Io, format!("cannot serialize manifest: {err}"))
        })?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// Records a mod, replacing any earlier entry for the same project.
    ///
    /// Files that came from a modpack have no project id, so they are matched by file
    /// name instead; otherwise every pack file would overwrite the previous one.
    pub fn upsert(&mut self, entry: Entry) {
        let slot = self.mods.iter_mut().find(|existing| {
            if entry.project_id.is_empty() {
                existing.project_id.is_empty() && existing.filename == entry.filename
            } else {
                existing.project_id == entry.project_id
            }
        });

        match slot {
            Some(existing) => *existing = entry,
            None => self.mods.push(entry),
        }
    }

    pub fn remove_project(&mut self, project_id: &str) -> Option<Entry> {
        let index = self
            .mods
            .iter()
            .position(|entry| entry.project_id == project_id)?;
        Some(self.mods.remove(index))
    }

    /// Finds an entry by slug, project id or file name, so `mod remove` accepts
    /// whatever the user remembers.
    pub fn find(&self, needle: &str) -> Option<&Entry> {
        let needle = needle.trim();
        self.mods.iter().find(|entry| {
            entry.project == needle
                || entry.project_id == needle
                || entry.filename == needle
                || entry
                    .filename
                    .strip_suffix(".jar")
                    .or_else(|| entry.filename.strip_suffix(".jar.disabled"))
                    .map(|stem| stem == needle)
                    .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(project: &str, id: &str, version: &str, file: &str) -> Entry {
        Entry {
            project: project.to_string(),
            project_id: id.to_string(),
            name: String::new(),
            version_id: version.to_string(),
            version_number: String::new(),
            filename: file.to_string(),
            sha1: String::new(),
        }
    }

    #[test]
    fn upsert_replaces_by_project_id() {
        let mut manifest = Manifest::new("1.21.1", "fabric");
        manifest.upsert(entry("sodium", "AANobbMI", "v1", "sodium-1.jar"));
        manifest.upsert(entry("sodium", "AANobbMI", "v2", "sodium-2.jar"));

        assert_eq!(manifest.mods.len(), 1);
        assert_eq!(manifest.mods[0].version_id, "v2");
    }

    #[test]
    fn entries_without_a_project_id_do_not_overwrite_each_other() {
        let mut manifest = Manifest::new("1.21.1", "fabric");
        let mut first = entry("a", "", "", "a.jar");
        first.project_id.clear();
        let mut second = entry("b", "", "", "b.jar");
        second.project_id.clear();

        manifest.upsert(first);
        manifest.upsert(second);

        assert_eq!(manifest.mods.len(), 2);
    }

    #[test]
    fn entries_are_found_by_slug_id_or_filename() {
        let mut manifest = Manifest::new("1.21.1", "fabric");
        manifest.upsert(entry("sodium", "AANobbMI", "v1", "sodium-fabric-1.0.jar"));

        assert!(manifest.find("sodium").is_some());
        assert!(manifest.find("AANobbMI").is_some());
        assert!(manifest.find("sodium-fabric-1.0.jar").is_some());
        assert!(manifest.find("sodium-fabric-1.0").is_some());
        assert!(manifest.find("lithium").is_none());
    }
}
