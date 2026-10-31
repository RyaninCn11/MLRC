//! Instances: a version that inherits another one and owns its game directory.
//!
//! This is the model the launcher we imitate uses instead of separate instances. Every
//! version already has its own directory (`versions/<id>/` doubles as the game
//! directory), so "one instance per mod set" becomes "one version per mod set": a instance
//! is a thin version document with `inheritsFrom`, plus a client jar hardlinked from
//! its parent. Removing it removes everything it owned.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::meta::version::VersionJson;
use crate::port::config::InstanceConfig;
use crate::port::state::{self, InstalledState};
use crate::port::DataRoot;

pub const FILE_NAME: &str = "instance.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// The version id of the instance, and its directory name.
    pub name: String,
    /// The version it inherits from.
    pub version: String,
    /// Heap used to live here; it moved to the instance's `java.xmx`, and this field exists
    /// only so an `instance.json` written by an earlier version still loads.
    #[serde(default, skip_serializing)]
    pub memory: String,
    /// Everything this instance overrides; an empty field inherits `config.toml`.
    #[serde(flatten)]
    pub overrides: InstanceConfig,
}

impl Instance {
    /// `<.minecraft>/versions/<name>/MLRC/instance.json`.
    pub fn path(root: &DataRoot, name: &str) -> PathBuf {
        root.state_dir(name).join(FILE_NAME)
    }

    /// Version ids become directory names, so keep them to the characters Mojang and
    /// the loaders already use.
    pub fn validate_name(name: &str) -> MlrcResult<()> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
            && !name.starts_with('.')
            && !name.contains("..");

        if valid {
            Ok(())
        } else {
            Err(MlrcError::new(
                ErrorCode::Usage,
                format!("invalid version name: {name} (letters, digits, '-', '_' and '.' only)"),
            )
            .with("name", name.to_string()))
        }
    }

    pub fn create(
        root: &DataRoot,
        name: &str,
        version: &str,
        overrides: InstanceConfig,
    ) -> MlrcResult<Self> {
        Self::validate_name(name)?;

        // A instance holds no files of its own, so the parent has to be installed.
        let parent = InstalledState::read(&state_path(root, version))?
            .filter(|state| state.complete)
            .ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::VersionUnknown,
                    format!("version {version} is not installed; run `mlrc install {version}`"),
                )
                .with("version", version.to_string())
            })?;

        let dir = root.version_dir(name);
        if dir.exists() {
            return Err(
                MlrcError::new(ErrorCode::Usage, format!("version {name} already exists"))
                    .with("version", name.to_string()),
            );
        }

        // A child document is only an identity plus a pointer; the rest is inherited.
        let document = format!(
            "{{\n  \"id\": \"{name}\",\n  \"type\": \"release\",\n  \
             \"inheritsFrom\": \"{version}\"\n}}\n"
        );
        VersionJson::store_cached(root, name, document.as_bytes())?;

        // The client jar is shared by hardlink, so the classpath builder needs no
        // special case for instances. A filesystem without hardlinks falls back to a copy.
        let parent_jar = root.version_dir(version).join(format!("{version}.jar"));
        let jar = dir.join(format!("{name}.jar"));
        if !jar.exists() && parent_jar.is_file() && std::fs::hard_link(&parent_jar, &jar).is_err() {
            std::fs::copy(&parent_jar, &jar)?;
        }

        let instance = Self {
            name: name.to_string(),
            version: version.to_string(),
            memory: String::new(),
            overrides,
        };

        std::fs::create_dir_all(root.state_dir(name))?;
        let state = InstalledState {
            id: name.to_string(),
            complete: true,
            java_component: parent.java_component,
            java_major: parent.java_major,
            files: 1,
            bytes: 0,
            installed_at: now(),
        };
        state.write(&state_path(root, name))?;
        // Nothing of its own is cached; the parent already lists what it pulled.
        state::write_objects(&root.state_dir(name), &Default::default())?;

        instance.save(root)?;
        Ok(instance)
    }

    pub fn save(&self, root: &DataRoot) -> MlrcResult<()> {
        let path = Self::path(root, &self.name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))?;
        std::fs::write(&path, text).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot write {}: {err}", path.display()),
            )
        })
    }

    pub fn load(root: &DataRoot, name: &str) -> MlrcResult<Option<Self>> {
        Self::validate_name(name)?;

        let path = Self::path(root, name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut instance: Self = serde_json::from_str(&text).map_err(|err| {
                    MlrcError::new(
                        ErrorCode::Config,
                        format!("invalid {}: {err}", path.display()),
                    )
                })?;
                // An older file kept the heap in `memory`; move it to the instance's layer.
                if instance.overrides.java.xmx.is_empty() && !instance.memory.trim().is_empty() {
                    if let Ok(value) = crate::launch::memory::normalize(&instance.memory) {
                        instance.overrides.java.xmx = value;
                    }
                }
                Ok(Some(instance))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    pub fn list(root: &DataRoot) -> MlrcResult<Vec<Self>> {
        let mut out = Vec::new();
        let dir = root.versions_dir();
        if !dir.is_dir() {
            return Ok(out);
        }

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(instance) = Self::load(root, &name)? {
                out.push(instance);
            }
        }

        out.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(out)
    }

    /// Removes the instance's version directory, which is everything it owned.
    pub fn remove(root: &DataRoot, name: &str) -> MlrcResult<bool> {
        Self::validate_name(name)?;

        if Self::load(root, name)?.is_none() {
            return Ok(false);
        }

        let dir = root.version_dir(name);

        // A running game can hold files open; retry instead of failing with errno 32
        // and no explanation.
        for attempt in 0..5 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => return Ok(true),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(_) if attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                Err(err) => {
                    return Err(MlrcError::new(
                        ErrorCode::Io,
                        format!("cannot remove {}: {err}", dir.display()),
                    )
                    .with("hint", "close the game if it is running, then retry"))
                }
            }
        }

        Ok(true)
    }

    /// The directory the game runs in: the version's own, always.
    pub fn game_dir(root: &DataRoot, name: &str) -> PathBuf {
        root.game_dir(name)
    }
}

/// `<versions>/<id>/MLRC/installed.json`.
fn state_path(root: &DataRoot, id: &str) -> PathBuf {
    root.state_dir(id).join(InstalledState::FILE_NAME)
}

fn now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_default()
}
