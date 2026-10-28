//! Named game directories.
//!
//! A version is shared: jars, libraries and assets live once under `data/`. An
//! instance only owns the part that is not shareable - the game directory with its
//! saves, options and mods - plus the version it points at.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::port::DataRoot;

pub const FILE_NAME: &str = "instance.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub version: String,
    /// `-Xmx` for this instance, empty to inherit the global setting.
    #[serde(default)]
    pub memory: String,
}

impl Instance {
    pub fn root_dir(root: &DataRoot, name: &str) -> PathBuf {
        root.instances_dir().join(name)
    }

    pub fn game_dir(root: &DataRoot, name: &str) -> PathBuf {
        Self::root_dir(root, name).join("minecraft")
    }

    pub fn config_path(root: &DataRoot, name: &str) -> PathBuf {
        Self::root_dir(root, name).join(FILE_NAME)
    }

    /// Names become directory names, so keep them boring.
    pub fn validate_name(name: &str) -> MlrcResult<()> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');

        if valid {
            Ok(())
        } else {
            Err(MlrcError::new(
                ErrorCode::Usage,
                format!("invalid instance name: {name} (letters, digits, '-' and '_' only)"),
            )
            .with("name", name.to_string()))
        }
    }

    pub fn create(root: &DataRoot, name: &str, version: &str, memory: &str) -> MlrcResult<Self> {
        Self::validate_name(name)?;
        let memory = if memory.is_empty() {
            String::new()
        } else {
            crate::launch::memory::normalize(memory)?
        };

        let instance = Self {
            name: name.to_string(),
            version: version.to_string(),
            memory,
        };

        std::fs::create_dir_all(Self::game_dir(root, name))?;
        let text = toml::to_string_pretty(&instance)
            .map_err(|err| MlrcError::new(ErrorCode::Io, format!("cannot serialize: {err}")))?;
        std::fs::write(Self::config_path(root, name), text)?;

        Ok(instance)
    }

    pub fn load(root: &DataRoot, name: &str) -> MlrcResult<Option<Self>> {
        Self::validate_name(name)?;

        let path = Self::config_path(root, name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let instance = toml::from_str(&text).map_err(|err| {
                    MlrcError::new(
                        ErrorCode::Config,
                        format!("invalid {}: {err}", path.display()),
                    )
                })?;
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
        let mut instances = Vec::new();

        let dir = root.instances_dir();
        if !dir.is_dir() {
            return Ok(instances);
        }

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(instance) = Self::load(root, &name)? {
                instances.push(instance);
            }
        }

        instances.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(instances)
    }

    pub fn remove(root: &DataRoot, name: &str) -> MlrcResult<bool> {
        Self::validate_name(name)?;

        let dir = Self::root_dir(root, name);
        if !dir.is_dir() {
            return Ok(false);
        }

        // A just-killed game, or an antivirus scan, can hold a handle for a moment.
        // Retrying briefly turns "file in use" into a clean delete instead of a
        // half-removed instance directory.
        let mut last_error = None;
        for attempt in 0..5 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => return Ok(true),
                Err(err) => {
                    last_error = Some(err);
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    }
                }
            }
        }

        Err(MlrcError::new(
            ErrorCode::Io,
            format!(
                "cannot remove {}: {}",
                dir.display(),
                last_error
                    .map(|err| err.to_string())
                    .unwrap_or_else(|| "unknown error".to_string())
            ),
        )
        .with("path", dir.display().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_restricted_to_path_safe_characters() {
        assert!(Instance::validate_name("survival").is_ok());
        assert!(Instance::validate_name("1-21-test_1").is_ok());
        assert!(Instance::validate_name("").is_err());
        assert!(Instance::validate_name("with space").is_err());
        assert!(Instance::validate_name("../escape").is_err());
        assert!(Instance::validate_name("a/b").is_err());
        // Dots are rejected on purpose: a name is a directory name, and `.` / `..`
        // must never be expressible.
        assert!(Instance::validate_name("..").is_err());
        assert!(Instance::validate_name("1.21").is_err());
    }
}
