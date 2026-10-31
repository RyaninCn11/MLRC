//! `config.toml`: the only file a user is expected to edit by hand.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Canonical key order, used by `mlrc config list`.
pub const KEYS: [&str; 18] = [
    "java.custom",
    "java.path",
    "java.xms",
    "java.xmx",
    "general.default_version",
    "general.mirror",
    "general.game_language",
    "general.proxy",
    "launch.brand",
    "launch.jvm_args",
    "launch.game_args",
    "launch.pre_launch",
    "auth.kind",
    "auth.server",
    "auth.player_name",
    "auth.nide8_id",
    "auth.nide8_jar",
    "curseforge.api_key",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub java: JavaConfig,
    pub general: GeneralConfig,
    pub launch: LaunchConfig,
    pub auth: AuthConfig,
    pub curseforge: CurseForgeConfig,
}

/// How the game is started, in the parts a launcher may influence.
///
/// Every field is "empty means unset", so the same struct serves the global file and a
/// fork's own overrides, and merging them is a field-by-field choice.
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
}

impl LaunchConfig {
    /// Per-field override: whatever `other` sets wins, everything else is inherited.
    pub fn over(&self, other: &Self) -> Self {
        Self {
            brand: pick(&other.brand, &self.brand),
            jvm_args: pick_list(&other.jvm_args, &self.jvm_args),
            game_args: pick_list(&other.game_args, &self.game_args),
            pre_launch: pick(&other.pre_launch, &self.pre_launch),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.brand.trim().is_empty()
            && self.jvm_args.is_empty()
            && self.game_args.is_empty()
            && self.pre_launch.trim().is_empty()
    }
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
/// `Auto` keeps the old behaviour: a stored session decides, and with none the game runs
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
    /// Pinned to this backend, but the stored session belongs to another.
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
    /// Yggdrasil API root, e.g. `https://littleskin.cn/api/yggdrasil`. Empty means
    /// offline mode, which is also the default.
    pub server: String,
    /// Name used in offline mode; an account's own name wins when one is stored.
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
            "launch.brand" => self.launch.brand.clone(),
            "launch.jvm_args" => self.launch.jvm_args.join(" "),
            "launch.game_args" => self.launch.game_args.join(" "),
            "launch.pre_launch" => self.launch.pre_launch.clone(),
            "auth.kind" => self.auth.kind.as_str().to_string(),
            "auth.server" => self.auth.server.clone(),
            "auth.player_name" => self.auth.player_name.clone(),
            "auth.nide8_id" => self.auth.nide8_id.clone(),
            "auth.nide8_jar" => self.auth.nide8_jar.clone(),
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
            "launch.brand" => {
                let trimmed = value.trim();
                // Empty or `none` restores the default shown by the launcher.
                self.launch.brand = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
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
                if key.trim() == "launch.game_args" {
                    self.launch.game_args = parsed;
                } else {
                    self.launch.jvm_args = parsed;
                }
            }
            "launch.pre_launch" => {
                let trimmed = value.trim();
                self.launch.pre_launch =
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                        String::new()
                    } else {
                        trimmed.to_string()
                    };
            }
            "auth.kind" => {
                self.auth.kind = AuthKind::parse(value).ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::Config,
                        format!("unknown login kind {value} (expected auto, offline, yggdrasil or nide8)"),
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

/// The one spelling a key has: `section.name`.
///
/// There are no aliases on purpose. Two spellings for one setting is how documentation
/// drifts - `gameLanguage` in one paragraph, `general.game_language` in the next - and
/// the second spelling always ends up either undocumented or wrong. Typing the old
/// camelCase form now produces an error that names the right key.
pub fn canonical(key: &str) -> MlrcResult<&'static str> {
    let key = key.trim();
    match KEYS.iter().find(|known| **known == key) {
        Some(known) => Ok(known),
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
    let needle = squash(key);
    if needle.is_empty() {
        return None;
    }

    KEYS.iter().copied().find(|known| {
        squash(known) == needle
            || known
                .split('.')
                .nth(1)
                .map(|tail| squash(tail) == needle)
                .unwrap_or(false)
    })
}

fn unknown_key(key: &str) -> MlrcError {
    let hint = match suggest(key) {
        Some(known) => format!("did you mean {known}?"),
        None => "run `mlrc config list` for the full list".to_string(),
    };

    MlrcError::new(ErrorCode::Config, format!("unknown config key {key}"))
        .with("hint", hint)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_round_trips() {
        for key in KEYS {
            assert!(canonical(key).is_ok(), "{key} must be accepted");
        }
    }

    #[test]
    fn keys_have_one_spelling() {
        assert_eq!(
            canonical("general.game_language").expect("dotted key"),
            "general.game_language"
        );
        assert!(canonical("gameLanguage").is_err());
        assert!(canonical("mirror").is_err());
        assert!(canonical(" xmx ").is_err());
    }

    #[test]
    fn a_habit_gets_a_suggestion() {
        assert_eq!(suggest("gameLanguage"), Some("general.game_language"));
        assert_eq!(suggest("Game_Language"), Some("general.game_language"));
        assert_eq!(suggest("mirror"), Some("general.mirror"));
        assert_eq!(suggest("xmx"), Some("java.xmx"));
        assert_eq!(suggest("whatever"), None);
    }

    #[test]
    fn the_login_kind_decides_the_backend() {
        use AuthKind::{Auto, Nide8, Offline, Yggdrasil};
        use Resolved;

        // Auto follows the stored session, and falls back to offline.
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
    fn launch_settings_round_trip() {
        let mut config = Config::default();
        config
            .set("launch.jvm_args", "-Xmx2G -Dfoo=bar")
            .expect("jvm args");
        assert_eq!(config.launch.jvm_args, vec!["-Xmx2G", "-Dfoo=bar"]);
        assert_eq!(
            config.get("launch.jvm_args").expect("get"),
            "-Xmx2G -Dfoo=bar"
        );

        config.set("launch.game_args", "--demo").expect("game args");
        assert_eq!(config.launch.game_args, vec!["--demo"]);

        config.set("launch.brand", "MyLauncher").expect("brand");
        assert_eq!(config.launch.brand, "MyLauncher");
        config.set("launch.brand", "none").expect("clear brand");
        assert!(config.launch.brand.is_empty());
    }

    #[test]
    fn a_fork_overrides_field_by_field() {
        let base = LaunchConfig {
            brand: "Base".to_string(),
            jvm_args: vec!["-Da".to_string()],
            game_args: vec!["--demo".to_string()],
            pre_launch: "echo base".to_string(),
        };
        let over = LaunchConfig {
            brand: String::new(),
            jvm_args: Vec::new(),
            game_args: vec!["--server".to_string()],
            pre_launch: String::new(),
        };

        let merged = base.over(&over);
        assert_eq!(merged.brand, "Base", "an empty field inherits");
        assert_eq!(merged.jvm_args, vec!["-Da"], "an empty list inherits");
        assert_eq!(merged.game_args, vec!["--server"], "a set field wins");
        assert_eq!(merged.pre_launch, "echo base");
    }

    #[test]
    fn setting_a_key_validates_before_writing() {
        let mut config = Config::default();
        config.set("auth.player_name", "Steve").expect("valid name");
        assert_eq!(config.auth.player_name, "Steve");
        assert!(config.set("auth.player_name", "bad name!").is_err());
        assert_eq!(
            config.auth.player_name, "Steve",
            "a rejected value is not stored"
        );

        config
            .set("general.mirror", "bmclapi")
            .expect("valid mirror");
        assert!(config.set("general.mirror", "fastest").is_err());
    }
}
