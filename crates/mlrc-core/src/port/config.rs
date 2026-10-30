//! `config.toml`: the only file a user is expected to edit by hand.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Canonical key order, used by `mlrc config list`.
pub const KEYS: [&str; 11] = [
    "java.custom",
    "java.path",
    "java.xms",
    "java.xmx",
    "general.default_version",
    "general.mirror",
    "general.game_language",
    "general.proxy",
    "auth.server",
    "auth.player_name",
    "curseforge.api_key",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub java: JavaConfig,
    pub general: GeneralConfig,
    pub auth: AuthConfig,
    pub curseforge: CurseForgeConfig,
}

/// Who the game says it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Yggdrasil API root, e.g. `https://littleskin.cn/api/yggdrasil`. Empty means
    /// offline mode, which is also the default.
    pub server: String,
    /// Name used in offline mode; an account's own name wins when one is stored.
    pub player_name: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            server: String::new(),
            player_name: "Player".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CurseForgeConfig {
    /// Eternal API key. Empty disables the source. `MLRC_CURSEFORGE_KEY` wins, so a
    /// key can stay out of the config file.
    pub api_key: String,
}

impl CurseForgeConfig {
    /// The key to send, from the environment first.
    pub fn key(&self) -> Option<String> {
        match std::env::var("MLRC_CURSEFORGE_KEY") {
            Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
            _ => {
                let value = self.api_key.trim();
                (!value.is_empty()).then(|| value.to_string())
            }
        }
    }
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
    /// `-Xms`; empty leaves Java's default alone.
    pub xms: String,
    /// `-Xmx`; empty means the launcher's 2G default.
    pub xmx: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Set by `mlrc use <version>`; used by a bare `mlrc launch`.
    pub default_version: String,
    pub mirror: Mirror,
    /// In-game language applied to `options.txt` before launch, e.g. `zh_cn`.
    /// Empty means "leave whatever the game has".
    pub game_language: String,
    /// `auto` (system settings), `none` (always direct) or `host:port`. Applies to the
    /// launcher's own downloads and to every JVM it starts.
    pub proxy: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_version: String::new(),
            mirror: Mirror::Auto,
            game_language: String::new(),
            proxy: "auto".to_string(),
        }
    }
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
            "java.xms" => self.java.xms.clone(),
            "java.xmx" => self.java.xmx.clone(),
            "general.default_version" => self.general.default_version.clone(),
            "general.mirror" => self.general.mirror.as_str().to_string(),
            "general.game_language" => self.general.game_language.clone(),
            "general.proxy" => {
                let proxy = self.general.proxy.trim();
                if proxy.is_empty() {
                    "auto".to_string()
                } else {
                    proxy.to_string()
                }
            }
            "auth.server" => self.auth.server.clone(),
            "auth.player_name" => self.auth.player_name.clone(),
            "curseforge.api_key" => self.curseforge.api_key.clone(),
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        match canonical(key)? {
            "java.custom" => self.java.custom = parse_bool(key, value)?,
            "java.path" => self.java.path = value.to_string(),
            "java.xms" | "java.xmx" => {
                // `default` and an empty value both mean "do not pass this flag".
                let cleared = value.is_empty() || value.eq_ignore_ascii_case("default");
                let stored = if cleared {
                    String::new()
                } else {
                    crate::launch::memory::normalize(value)?
                };
                if key.trim().ends_with("xms") || key.eq_ignore_ascii_case("xms") {
                    self.java.xms = stored;
                } else {
                    self.java.xmx = stored;
                }
            }
            "auth.server" => {
                // `none` clears it, because an empty shell argument is awkward to pass.
                let cleared = value.trim().is_empty() || value.eq_ignore_ascii_case("none");
                self.auth.server = if cleared {
                    String::new()
                } else {
                    crate::auth::validate_server(value)?
                };
            }
            "auth.player_name" => {
                let name = value.trim();
                crate::auth::offline::validate_name(name)?;
                self.auth.player_name = name.to_string();
            }
            "curseforge.api_key" => {
                // `none` clears it, because an empty shell argument is awkward to pass.
                let cleared = value.trim().is_empty() || value.eq_ignore_ascii_case("none");
                self.curseforge.api_key = if cleared {
                    String::new()
                } else {
                    value.trim().to_string()
                };
            }
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
            "general.game_language" => {
                // Empty shells are awkward to pass through a CLI, so `none` is an
                // explicit way to clear the setting.
                let cleared = value.is_empty() || value.eq_ignore_ascii_case("none");
                if !cleared {
                    crate::launch::options::validate_language(value)?;
                }
                self.general.game_language = if cleared {
                    String::new()
                } else {
                    value.to_string()
                };
            }
            "general.proxy" => {
                // Validation happens here so a typo is caught by `config set`, not by
                // the next download.
                crate::fetch::proxy::choice(value)?;
                self.general.proxy = if value.trim().is_empty() {
                    "auto".to_string()
                } else {
                    value.trim().to_string()
                };
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
        "java.xms" | "xms" => "java.xms",
        "java.xmx" | "xmx" | "memory" => "java.xmx",
        "general.default_version" | "defaultVersion" | "defaultversion" => {
            "general.default_version"
        }
        "general.mirror" | "mirror" => "general.mirror",
        "general.game_language" | "gameLanguage" | "gamelanguage" => "general.game_language",
        "general.proxy" | "proxy" => "general.proxy",
        "auth.server" | "authServer" | "yggdrasil" | "authserver" => "auth.server",
        "auth.player_name" | "playerName" | "playername" | "username" => "auth.player_name",
        "curseforge.api_key" | "curseforge.apiKey" | "curseforgeApiKey" | "curseforgeKey" => {
            "curseforge.api_key"
        }
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
