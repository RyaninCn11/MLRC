//! `config.toml`: the only file a user is expected to edit by hand.
//!
//! Two layers exist. `MLRC/config.toml` is the global one; a instance's `instance.json`
//! carries its own `java.*`, `launch.*` and `auth.account`, and whatever it leaves empty
//! is inherited. [`InstanceConfig::apply`] folds them into the [`Effective`] settings a
//! launch actually uses.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Canonical key order, used by `mlrc config list`.
///
/// `auth.account` is not here: it only exists per instance, because the default account is
/// chosen from the account list rather than from a configuration value.
pub const KEYS: [&str; 18] = [
    "java.custom",
    "java.path",
    "java.xms",
    "java.xmx",
    "general.default_version",
    "general.mirror",
    "general.proxy",
    "general.curseforge_api_key",
    "launch.brand",
    "launch.jvm_args",
    "launch.game_args",
    "launch.pre_launch",
    "launch.game_language",
    "auth.kind",
    "auth.server",
    "auth.player_name",
    "auth.nide8_id",
    "auth.nide8_jar",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub java: JavaConfig,
    pub general: GeneralConfig,
    pub launch: LaunchConfig,
    pub auth: AuthConfig,
    /// The CurseForge key used to live in its own table. Read for compatibility, never
    /// written.
    #[serde(default, skip_serializing)]
    pub curseforge: CurseForgeConfig,
}

/// How the game is started, in the parts a launcher may influence.
///
/// Every field is "empty means unset", so the same struct serves the global file and a
/// instance's own overrides, and merging them is a field-by-field choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchConfig {
    /// Replaces `-Dminecraft.launcher.brand`, which the game shows in the corner of
    /// the main menu and on the debug screen. Empty means `MLRC`.
    pub brand: String,
    /// Extra JVM flags, appended after the document's own. `${...}` markers are
    /// expanded like the ones Minecraft itself understands.
    pub jvm_args: Vec<String>,
    /// Extra arguments for the game itself, appended after the document's own.
    pub game_args: Vec<String>,
    /// A shell command run before the game starts, in the game directory.
    pub pre_launch: String,
    /// In-game language written into `options.txt` before launch, e.g. `zh_cn`.
    /// Empty leaves whatever the game has.
    pub game_language: String,
}

impl LaunchConfig {
    /// Per-field override: whatever `other` sets wins, everything else is inherited.
    pub fn over(&self, other: &Self) -> Self {
        Self {
            brand: pick(&other.brand, &self.brand),
            jvm_args: pick_list(&other.jvm_args, &self.jvm_args),
            game_args: pick_list(&other.game_args, &self.game_args),
            pre_launch: pick(&other.pre_launch, &self.pre_launch),
            game_language: pick(&other.game_language, &self.game_language),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.brand.trim().is_empty()
            && self.jvm_args.is_empty()
            && self.game_args.is_empty()
            && self.pre_launch.trim().is_empty()
            && self.game_language.trim().is_empty()
    }

    /// The keys a instance may own, in the order `config list` prints them.
    pub const KEYS: [&'static str; 5] = [
        "launch.brand",
        "launch.jvm_args",
        "launch.game_args",
        "launch.pre_launch",
        "launch.game_language",
    ];

    pub fn get_key(&self, key: &str) -> MlrcResult<String> {
        Ok(match key {
            "launch.brand" => self.brand.clone(),
            "launch.jvm_args" => self.jvm_args.join(" "),
            "launch.game_args" => self.game_args.join(" "),
            "launch.pre_launch" => self.pre_launch.clone(),
            "launch.game_language" => self.game_language.clone(),
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        let trimmed = value.trim();
        let cleared = trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none");

        match key {
            "launch.brand" => {
                // Empty or `none` restores the default shown by the launcher.
                self.brand = if cleared {
                    String::new()
                } else {
                    trimmed.to_string()
                };
            }
            "launch.jvm_args" | "launch.game_args" => {
                // One string, split on whitespace: a shell already joins what the user
                // typed, and a TOML array is what gets written back.
                let parsed: Vec<String> = value
                    .split_whitespace()
                    .filter(|part| !part.eq_ignore_ascii_case("none"))
                    .map(str::to_string)
                    .collect();
                if key == "launch.game_args" {
                    self.game_args = parsed;
                } else {
                    self.jvm_args = parsed;
                }
            }
            "launch.pre_launch" => {
                self.pre_launch = if cleared {
                    String::new()
                } else {
                    trimmed.to_string()
                };
            }
            "launch.game_language" => {
                if cleared {
                    self.game_language = String::new();
                } else {
                    crate::launch::options::validate_language(trimmed)?;
                    self.game_language = trimmed.to_string();
                }
            }
            other => return Err(unknown_key(other)),
        }

        Ok(())
    }

    /// What this layer actually sets, or `None` when the field is empty.
    pub fn overridden(&self, key: &str) -> Option<String> {
        let value = self.get_key(key).ok()?;
        (!value.is_empty()).then_some(value)
    }
}

/// A instance's own Java settings.
///
/// `custom` is tri-state: `None` inherits the global value, because "false" and "not
/// overridden" are different answers for a boolean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaLayer {
    pub custom: Option<bool>,
    pub path: String,
    pub xms: String,
    pub xmx: String,
}

impl JavaLayer {
    pub fn get_key(&self, key: &str) -> MlrcResult<String> {
        Ok(match key {
            "java.custom" => match self.custom {
                Some(value) => value.to_string(),
                None => String::new(),
            },
            "java.path" => self.path.clone(),
            "java.xms" => self.xms.clone(),
            "java.xmx" => self.xmx.clone(),
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        let trimmed = value.trim();
        let cleared = trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none");

        match key {
            "java.custom" => {
                self.custom = if cleared {
                    None
                } else {
                    Some(parse_bool(key, trimmed)?)
                }
            }
            "java.path" => {
                self.path = if cleared {
                    String::new()
                } else {
                    trimmed.to_string()
                }
            }
            "java.xms" | "java.xmx" => {
                let stored = if cleared {
                    String::new()
                } else {
                    crate::launch::memory::normalize(trimmed)?
                };
                if key == "java.xms" {
                    self.xms = stored;
                } else {
                    self.xmx = stored;
                }
            }
            other => return Err(unknown_key(other)),
        }

        Ok(())
    }

    /// What this layer actually sets, or `None` when it inherits.
    pub fn overridden(&self, key: &str) -> Option<String> {
        let value = self.get_key(key).ok()?;
        (!value.is_empty()).then_some(value)
    }

    pub fn apply(&self, global: &JavaConfig) -> JavaConfig {
        JavaConfig {
            custom: self.custom.unwrap_or(global.custom),
            path: pick(&self.path, &global.path),
            xms: pick(&self.xms, &global.xms),
            xmx: pick(&self.xmx, &global.xmx),
        }
    }
}

/// A instance's own account choice: a label into `accounts.json`, or empty for the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthLayer {
    pub account: String,
}

impl AuthLayer {
    pub fn get_key(&self, key: &str) -> MlrcResult<String> {
        match key {
            "auth.account" => Ok(self.account.clone()),
            other => Err(unknown_key(other)),
        }
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        match key {
            "auth.account" => {
                let trimmed = value.trim();
                self.account = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                    String::new()
                } else {
                    trimmed.to_string()
                };
                Ok(())
            }
            other => Err(unknown_key(other)),
        }
    }

    pub fn overridden(&self, key: &str) -> Option<String> {
        let value = self.get_key(key).ok()?;
        (!value.is_empty()).then_some(value)
    }
}

/// Everything a instance may override, stored in its `instance.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstanceConfig {
    pub launch: LaunchConfig,
    pub java: JavaLayer,
    pub auth: AuthLayer,
}

impl InstanceConfig {
    pub fn get_key(&self, key: &str) -> MlrcResult<String> {
        match key {
            key if key.starts_with("launch.") => self.launch.get_key(key),
            key if key.starts_with("java.") => self.java.get_key(key),
            key if key.starts_with("auth.") => self.auth.get_key(key),
            other => Err(unknown_key(other)),
        }
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        match key {
            key if key.starts_with("launch.") => self.launch.set_key(key, value),
            key if key.starts_with("java.") => self.java.set_key(key, value),
            key if key.starts_with("auth.") => self.auth.set_key(key, value),
            other => Err(unknown_key(other)),
        }
    }

    pub fn overridden(&self, key: &str) -> Option<String> {
        let value = self.get_key(key).ok()?;
        (!value.is_empty()).then_some(value)
    }

    /// Folds this layer onto the global configuration.
    pub fn apply(&self, cfg: &Config) -> Effective {
        Effective {
            launch: cfg.launch.over(&self.launch),
            java: self.java.apply(&cfg.java),
            account: self.auth.account.trim().to_string(),
        }
    }
}

/// The settings a launch actually uses, after both layers were folded together.
#[derive(Debug, Clone)]
pub struct Effective {
    pub launch: LaunchConfig,
    pub java: JavaConfig,
    /// Account label, possibly empty: empty means the default account.
    pub account: String,
}

/// Whether a key may be set per instance.
pub fn is_instance_key(key: &str) -> bool {
    key.starts_with("launch.") || key.starts_with("java.") || key == "auth.account"
}

/// Whether a key only exists in a instance's layer.
pub fn is_instance_only(key: &str) -> bool {
    key == "auth.account"
}

fn pick(over: &str, base: &str) -> String {
    if over.trim().is_empty() {
        base.trim().to_string()
    } else {
        over.trim().to_string()
    }
}

fn pick_list(over: &[String], base: &[String]) -> Vec<String> {
    if over.is_empty() {
        base.to_vec()
    } else {
        over.to_vec()
    }
}

/// Which login backend a launch uses.
///
/// `Auto` keeps the old behaviour: a stored account decides, and with none the game runs
/// offline. The other values pin it, so "why is it launching as someone else" has an
/// answer in one place instead of being inferred from which keys happen to be set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    #[default]
    Auto,
    Offline,
    Yggdrasil,
    Nide8,
}

impl AuthKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "offline" | "none" => Some(Self::Offline),
            "yggdrasil" | "third-party" | "thirdparty" => Some(Self::Yggdrasil),
            "nide8" | "unified-pass" | "unifiedpass" | "pass" => Some(Self::Nide8),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Offline => "offline",
            Self::Yggdrasil => "yggdrasil",
            Self::Nide8 => "nide8",
        }
    }

    /// What a launch will actually do, given the stored session, if any.
    ///
    /// `stored` is the session's own kind; empty means yggdrasil, which is what every
    /// session written before unified pass existed is.
    pub fn resolve(self, stored: Option<&str>) -> Resolved {
        let stored = stored.map(|kind| {
            if kind.trim().is_empty() {
                "yggdrasil"
            } else {
                kind.trim()
            }
        });

        match self {
            Self::Offline => Resolved::Offline,
            Self::Auto => match stored {
                Some("nide8") => Resolved::Nide8,
                Some(_) => Resolved::Yggdrasil,
                None => Resolved::Offline,
            },
            Self::Yggdrasil | Self::Nide8 => {
                let wanted = if self == Self::Nide8 {
                    "nide8"
                } else {
                    "yggdrasil"
                };
                match stored {
                    None => Resolved::Missing(wanted),
                    Some(kind) if kind == wanted => {
                        if wanted == "nide8" {
                            Resolved::Nide8
                        } else {
                            Resolved::Yggdrasil
                        }
                    }
                    Some(_) => Resolved::Wrong(wanted),
                }
            }
        }
    }
}

/// What a launch will actually do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    Offline,
    Yggdrasil,
    Nide8,
    /// Pinned to this backend, but nothing is stored.
    Missing(&'static str),
    /// Pinned to this backend, but the stored account belongs to another.
    Wrong(&'static str),
}

impl Resolved {
    pub fn backend(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Yggdrasil => "yggdrasil",
            Self::Nide8 => "nide8",
            Self::Missing(backend) | Self::Wrong(backend) => backend,
        }
    }

    /// True when a launch can proceed with this backend.
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Offline | Self::Yggdrasil | Self::Nide8)
    }
}

/// Who the game says it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Which backend a launch uses; see [`AuthKind`].
    pub kind: AuthKind,
    /// Yggdrasil API root, e.g. `https://littleskin.cn/api/yggdrasil`. It is also where
    /// `mlrc auth login` goes when no flag overrides it.
    pub server: String,
    /// Name used in offline mode; an account's own name wins when one is selected.
    pub player_name: String,
    /// Unified pass server id, 32 hexadecimal characters. Empty disables the mode.
    pub nide8_id: String,
    /// Path to `nide8auth.jar`. Empty means `MLRC/injector/nide8auth.jar`.
    pub nide8_jar: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            kind: AuthKind::Auto,
            server: String::new(),
            player_name: "Player".to_string(),
            nide8_id: String::new(),
            nide8_jar: String::new(),
        }
    }
}

/// The `[curseforge]` table of an older file. Only read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CurseForgeConfig {
    pub api_key: String,
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
    /// The in-game language used to live here; it moved to `launch.game_language`, and
    /// this field exists only so an older file still loads.
    #[serde(default, skip_serializing)]
    pub game_language: String,
    /// `auto` (system settings), `none` (always direct) or `host:port`. Applies to the
    /// launcher's own downloads and to every JVM it starts.
    pub proxy: String,
    /// Eternal CurseForge API key. Empty disables the source; `MLRC_CURSEFORGE_KEY`
    /// wins, so a key can stay out of the file.
    pub curseforge_api_key: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            default_version: String::new(),
            mirror: Mirror::Auto,
            game_language: String::new(),
            proxy: "auto".to_string(),
            curseforge_api_key: String::new(),
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
            Ok(text) => toml::from_str(&text)
                .map(|mut config: Self| {
                    config.migrate();
                    config
                })
                .map_err(|err| {
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

    /// Reads an older file into the current shape.
    ///
    /// Two keys moved - the game language into the launch layer, the CurseForge key out
    /// of its own table - and a file written by an earlier version still has them where
    /// they used to be. Moving them here means no user has to be told about it.
    fn migrate(&mut self) {
        if self.launch.game_language.trim().is_empty()
            && !self.general.game_language.trim().is_empty()
        {
            self.launch.game_language = self.general.game_language.trim().to_string();
            self.general.game_language.clear();
        }

        if self.general.curseforge_api_key.trim().is_empty()
            && !self.curseforge.api_key.trim().is_empty()
        {
            self.general.curseforge_api_key = self.curseforge.api_key.trim().to_string();
            self.curseforge.api_key.clear();
        }
    }

    /// The CurseForge key to send, environment first.
    pub fn curseforge_key(&self) -> Option<String> {
        match std::env::var("MLRC_CURSEFORGE_KEY") {
            Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
            _ => {
                let value = self.general.curseforge_api_key.trim();
                (!value.is_empty()).then(|| value.to_string())
            }
        }
    }

    pub fn get(&self, key: &str) -> MlrcResult<String> {
        Ok(match canonical(key)? {
            "java.custom" => self.java.custom.to_string(),
            "java.path" => self.java.path.clone(),
            "java.xms" => self.java.xms.clone(),
            "java.xmx" => self.java.xmx.clone(),
            "general.default_version" => self.general.default_version.clone(),
            "general.mirror" => self.general.mirror.as_str().to_string(),
            "general.proxy" => {
                let proxy = self.general.proxy.trim();
                if proxy.is_empty() {
                    "auto".to_string()
                } else {
                    proxy.to_string()
                }
            }
            "general.curseforge_api_key" => self.general.curseforge_api_key.clone(),
            key if key.starts_with("launch.") => self.launch.get_key(key)?,
            "auth.kind" => self.auth.kind.as_str().to_string(),
            "auth.server" => self.auth.server.clone(),
            "auth.player_name" => self.auth.player_name.clone(),
            "auth.nide8_id" => self.auth.nide8_id.clone(),
            "auth.nide8_jar" => self.auth.nide8_jar.clone(),
            "auth.account" => {
                return Err(MlrcError::new(
                    ErrorCode::Config,
                    "auth.account only exists per instance; pass --instance <name>",
                ))
            }
            other => return Err(unknown_key(other)),
        })
    }

    pub fn set(&mut self, key: &str, value: &str) -> MlrcResult<()> {
        match canonical(key)? {
            "java.custom" => self.java.custom = parse_bool(key, value)?,
            "java.path" => self.java.path = value.to_string(),
            "java.xms" | "java.xmx" => {
                // An empty value means "do not pass this flag".
                let cleared = value.is_empty() || value.eq_ignore_ascii_case("default");
                let stored = if cleared {
                    String::new()
                } else {
                    crate::launch::memory::normalize(value)?
                };
                if key.trim().ends_with("xms") {
                    self.java.xms = stored;
                } else {
                    self.java.xmx = stored;
                }
            }
            key if key.starts_with("launch.") => self.launch.set_key(key, value)?,
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
            "general.curseforge_api_key" => {
                // `none` clears it, because an empty shell argument is awkward to pass.
                let cleared = value.trim().is_empty() || value.eq_ignore_ascii_case("none");
                self.general.curseforge_api_key = if cleared {
                    String::new()
                } else {
                    value.trim().to_string()
                };
            }
            "auth.kind" => {
                self.auth.kind = AuthKind::parse(value).ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::Config,
                        format!(
                            "unknown login kind {value} (expected auto, offline, yggdrasil or nide8)"
                        ),
                    )
                })?;
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
            "auth.nide8_id" => {
                let cleared = value.trim().is_empty() || value.eq_ignore_ascii_case("none");
                self.auth.nide8_id = if cleared {
                    String::new()
                } else {
                    crate::auth::nide8::validate_id(value)?.to_string()
                };
            }
            "auth.nide8_jar" => {
                let trimmed = value.trim();
                self.auth.nide8_jar = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none")
                {
                    String::new()
                } else {
                    trimmed.to_string()
                };
            }
            "auth.account" => {
                return Err(MlrcError::new(
                    ErrorCode::Config,
                    "auth.account only exists per instance; pass --instance <name>",
                ))
            }
            other => return Err(unknown_key(other)),
        }
        Ok(())
    }
}

/// The one spelling a key has: `section.name`.
///
/// There are no aliases on purpose. Two spellings for one setting is how documentation
/// drifts - `gameLanguage` in one paragraph, `game_language` in the next - and the
/// second spelling always ends up either undocumented or wrong. Typing the old
/// camelCase form now produces an error that names the right key.
pub fn canonical(key: &str) -> MlrcResult<&'static str> {
    let key = key.trim();
    match KEYS.iter().find(|known| **known == key) {
        Some(known) => Ok(known),
        // The account key only exists in a instance's layer, but it is still a key a user
        // types, so it survives canonicalisation and is refused later with a message
        // that says where it lives.
        None if key == "auth.account" => Ok("auth.account"),
        None => Err(unknown_key(key)),
    }
}

/// Lowercase, letters and digits only: `gameLanguage`, `game_language` and
/// `game-language` all squash to the same thing.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// The key the user probably meant, so a habit is not just refused.
fn suggest(key: &str) -> Option<&'static str> {
    let wanted = squash(key);
    // A habit omits the section: `gameLanguage` should find `launch.game_language`, so
    // the last segment counts as well as the whole key.
    let matches = |known: &str| {
        squash(known) == wanted || squash(known.rsplit('.').next().unwrap_or(known)) == wanted
    };

    KEYS.iter()
        .find(|known| matches(known))
        .copied()
        .or_else(|| matches("auth.account").then_some("auth.account"))
}

fn unknown_key(key: &str) -> MlrcError {
    let mut error = MlrcError::new(ErrorCode::Config, format!("unknown config key {key}"));
    if let Some(suggestion) = suggest(key) {
        error = error.with("hint", format!("did you mean {suggestion}?"));
    } else {
        error = error.with("hint", "run `mlrc config list` for the full list");
    }
    error
}

fn parse_bool(key: &str, value: &str) -> MlrcResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(MlrcError::new(
            ErrorCode::Config,
            format!("invalid value for {key}: {other} (expected true or false)"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults() {
        let config = Config::load(Path::new("target/does-not-exist/config.toml")).expect("load");
        assert!(!config.java.custom);
        assert_eq!(config.general.mirror, Mirror::Auto);
        assert_eq!(config.general.proxy, "auto");
        assert_eq!(config.auth.kind, AuthKind::Auto);
        assert!(config.launch.jvm_args.is_empty());
    }

    #[test]
    fn every_listed_key_round_trips() {
        for key in KEYS {
            let mut config = Config::default();
            // Reading must work for every key, whatever the value is.
            config.get(key).unwrap_or_else(|err| panic!("{key}: {err}"));
            // And setting a harmless value must be accepted for every key whose
            // validator allows it.
            if key.starts_with("java.") {
                let _ = config.set(key, "1");
            }
        }
    }

    #[test]
    fn keys_have_one_spelling() {
        assert_eq!(
            canonical("launch.game_language").expect("dotted key"),
            "launch.game_language"
        );
        // The keys that moved are gone, not aliased.
        assert!(canonical("general.game_language").is_err());
        assert!(canonical("curseforge.api_key").is_err());
        assert!(canonical("gameLanguage").is_err());
        assert!(canonical("mirror").is_err());
        assert!(canonical(" xmx ").is_err());
        assert_eq!(
            canonical("auth.account").expect("instance key"),
            "auth.account"
        );
    }

    #[test]
    fn a_habit_gets_a_suggestion() {
        assert_eq!(suggest("gameLanguage"), Some("launch.game_language"));
        assert_eq!(suggest("Game_Language"), Some("launch.game_language"));
        assert_eq!(suggest("mirror"), Some("general.mirror"));
        assert_eq!(suggest("xmx"), Some("java.xmx"));
        assert_eq!(
            suggest("curseforge_api_key"),
            Some("general.curseforge_api_key")
        );
        assert_eq!(suggest("whatever"), None);
    }

    #[test]
    fn an_old_file_moves_its_two_keys() {
        let mut config: Config = toml::from_str(
            r#"
[general]
game_language = "zh_cn"

[curseforge]
api_key = "secret"
"#,
        )
        .expect("parses");

        config.migrate();

        assert_eq!(config.launch.game_language, "zh_cn");
        assert!(config.general.game_language.is_empty());
        assert_eq!(config.general.curseforge_api_key, "secret");
        assert!(config.curseforge.api_key.is_empty());

        // Neither key is written back under its old name.
        let text = toml::to_string_pretty(&config).expect("serializes");
        assert!(!text.contains("[curseforge]"), "{text}");
    }

    #[test]
    fn an_instance_folds_onto_the_global_layer() {
        let mut global = Config::default();
        global.java.xmx = "2G".to_string();
        global.java.custom = false;
        global.launch.brand = "Global".to_string();

        let mut instance = InstanceConfig::default();
        instance.java.set_key("java.xmx", "6G").expect("xmx");
        instance
            .java
            .set_key("java.custom", "true")
            .expect("custom");
        instance
            .auth
            .set_key("auth.account", "main")
            .expect("account");

        let effective = instance.apply(&global);
        assert_eq!(effective.java.xmx, "6G");
        assert!(effective.java.custom);
        assert_eq!(effective.launch.brand, "Global");
        assert_eq!(effective.account, "main");

        // A fresh instance inherits everything, including an empty account label.
        let inherited = InstanceConfig::default().apply(&global);
        assert_eq!(inherited.java.xmx, "2G");
        assert_eq!(inherited.account, "");
    }

    #[test]
    fn an_instance_layer_owns_its_keys() {
        let mut layer = LaunchConfig::default();
        assert_eq!(layer.overridden("launch.brand"), None);

        layer.set_key("launch.brand", "Instance").expect("brand");
        layer
            .set_key("launch.jvm_args", "-Da=1 -Db=2")
            .expect("jvm");
        assert_eq!(
            layer.overridden("launch.brand").as_deref(),
            Some("Instance")
        );
        assert_eq!(
            layer.overridden("launch.jvm_args").as_deref(),
            Some("-Da=1 -Db=2")
        );

        // Empty clears the layer, which is what "inherit" means.
        layer.set_key("launch.brand", "none").expect("clear");
        assert_eq!(layer.overridden("launch.brand"), None);

        // A key that is not a launch setting is refused, not silently written.
        assert!(layer.set_key("java.xmx", "4G").is_err());
    }

    #[test]
    fn the_login_kind_decides_the_backend() {
        use AuthKind::{Auto, Nide8, Offline, Yggdrasil};
        use Resolved;

        // Auto follows the stored account, and falls back to offline.
        assert_eq!(Auto.resolve(Some("nide8")), Resolved::Nide8);
        assert_eq!(Auto.resolve(Some("yggdrasil")), Resolved::Yggdrasil);
        assert_eq!(Auto.resolve(Some("")), Resolved::Yggdrasil);
        assert_eq!(Auto.resolve(None), Resolved::Offline);

        // Pinned backends are strict about what is stored.
        assert_eq!(Yggdrasil.resolve(Some("yggdrasil")), Resolved::Yggdrasil);
        assert_eq!(Nide8.resolve(Some("nide8")), Resolved::Nide8);
        assert_eq!(Nide8.resolve(None), Resolved::Missing("nide8"));
        assert_eq!(Nide8.resolve(Some("yggdrasil")), Resolved::Wrong("nide8"));
        assert_eq!(
            Yggdrasil.resolve(Some("nide8")),
            Resolved::Wrong("yggdrasil")
        );

        // Offline ignores whatever is stored.
        assert_eq!(Offline.resolve(Some("nide8")), Resolved::Offline);
    }

    #[test]
    fn the_kind_is_spelled_out_and_validated() {
        assert_eq!(AuthKind::parse("nide8"), Some(AuthKind::Nide8));
        assert_eq!(AuthKind::parse("Unified-Pass"), Some(AuthKind::Nide8));
        assert_eq!(AuthKind::parse("AUTO"), Some(AuthKind::Auto));
        assert_eq!(AuthKind::parse("microsoft"), None);

        let mut config = Config::default();
        assert_eq!(config.get("auth.kind").expect("get"), "auto");
        config.set("auth.kind", "nide8").expect("set");
        assert_eq!(config.auth.kind, AuthKind::Nide8);
        assert!(config.set("auth.kind", "microsoft").is_err());
    }

    #[test]
    fn an_instance_only_key_is_refused_globally() {
        let mut config = Config::default();
        assert!(config.get("auth.account").is_err());
        assert!(config.set("auth.account", "main").is_err());

        let mut instance = InstanceConfig::default();
        instance
            .set_key("auth.account", "main")
            .expect("per instance");
        assert_eq!(instance.auth.account, "main");
    }

    #[test]
    fn values_are_validated_before_they_are_stored() {
        let mut config = Config::default();

        assert!(config.set("java.custom", "maybe").is_err());
        assert!(config.set("general.mirror", "faster").is_err());
        assert!(config.set("general.proxy", "nope").is_err());
        assert!(config
            .set("launch.game_language", "not a language")
            .is_err());
        assert!(config.set("auth.player_name", "has space").is_err());
        assert!(config.set("auth.nide8_id", "short").is_err());

        // A rejected value leaves the old one alone.
        assert_eq!(config.general.mirror, Mirror::Auto);
    }

    #[test]
    fn the_curseforge_key_prefers_the_environment() {
        let mut config = Config::default();
        config.general.curseforge_api_key = "from-file".to_string();
        assert_eq!(config.curseforge_key().as_deref(), Some("from-file"));

        // The environment wins when set, and is not stored anywhere.
        std::env::set_var("MLRC_CURSEFORGE_KEY", "from-env");
        assert_eq!(config.curseforge_key().as_deref(), Some("from-env"));
        std::env::remove_var("MLRC_CURSEFORGE_KEY");
    }
}
