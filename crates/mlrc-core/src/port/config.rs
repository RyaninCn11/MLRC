//! `config.toml`: the only file a user is expected to edit by hand.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Canonical key order, used by `mlrc config list`.
pub const KEYS: [&str; 4] = [
    "java.custom",
    "java.path",
    "general.default_version",
    "general.mirror",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub java: JavaConfig,
    pub general: GeneralConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaConfig {
    /// When false - the default - the host JDK (`JAVA_HOME`, `PATH`) is ignored
    /// entirely and only MLRC-managed runtimes are used. This is what keeps a
    /// system JDK upgrade from breaking an installed Minecraft version.
    pub custom: bool,
    /// Explicit java binary. Wins over everything when set.
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Set by `mlrc use <version>`; used by a bare `mlrc launch`.
    pub default_version: String,
    pub mirror: Mirror,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mirror {
    #[default]
    Auto,
    Official,
    Bmclapi,
}

impl Mirror {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Official => "official",
            Self::Bmclapi => "bmclapi",
        }
    }
}

impl Config {
    /// Missing file is not an error: defaults are a valid configuration.
    pub fn load(path: &Path) -> MlrcResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Config,
                    format!("invalid config {}: {err}", path.display()),
                )
                .with("path", path.display().to_string())
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    pub fn save(&self, path: &Path) -> MlrcResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot create {}: {err}", parent.display()),
                )
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(|err| {
            MlrcError::new(ErrorCode::Config, format!("cannot serialize config: {err}"))
        })?;
        std::fs::write(path, text).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot write {}: {err}", path.display()),
            )
        })
    }

    pub fn get(&self, key: &str) -> MlrcResult<String> {
        Ok(match canonical(key)? {
            "java.custom" => self.java.custom.to_string(),
            "java.path" => self.java.path.clone(),
            "general.default_version" => self.general.default_version.clone(),
            "general.mirror" => self.general.mirror.as_str().to_string(),
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        match canonical(key)? {
            "java.custom" => self.java.custom = parse_bool(key, value)?,
            "java.path" => self.java.path = value.to_string(),
            "general.default_version" => self.general.default_version = value.to_string(),
            "general.mirror" => {
                self.general.mirror = match value.to_ascii_lowercase().as_str() {
                    "auto" => Mirror::Auto,
                    "official" => Mirror::Official,
                    "bmclapi" => Mirror::Bmclapi,
                    other => {
                        return Err(MlrcError::new(
                            ErrorCode::Config,
                            format!(
                            "invalid value for {key}: {other} (expected auto, official, bmclapi)"
                        ),
                        ))
                    }
                }
            }
            other => return Err(unknown_key(other)),
        }
        Ok(())
    }
}

/// Accepts both the canonical dotted key and the camelCase spelling users type.
pub fn canonical(key: &str) -> MlrcResult<&'static str> {
    Ok(match key.trim() {
        "java.custom" | "customJava" | "customjava" => "java.custom",
        "java.path" | "javaPath" | "javapath" => "java.path",
        "general.default_version" | "defaultVersion" | "defaultversion" => {
            "general.default_version"
        }
        "general.mirror" | "mirror" => "general.mirror",
        other => return Err(unknown_key(other)),
    })
}

fn unknown_key(key: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::Config,
        format!(
            "unknown config key: {key} (known keys: {})",
            KEYS.join(", ")
        ),
    )
    .with("key", key.to_string())
}

fn parse_bool(key: &str, value: &str) -> MlrcResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(MlrcError::new(
            ErrorCode::Config,
            format!("invalid boolean for {key}: {other} (expected true or false)"),
        )),
    }
}
