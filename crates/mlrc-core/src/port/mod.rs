//! Where MLRC keeps its data, and how it decides that without asking anyone.
//!
//! The layout follows the launcher this project imitates: `.minecraft` holds
//! everything the game and other tools understand, `MLRC` holds everything only this
//! launcher needs. Both live beside the executable, which is what keeps the
//! installation a folder you can move.

pub mod config;
pub mod state;

use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

pub const ENV_DATA_DIR: &str = "MLRC_DATA_DIR";
const PROBE_FILE: &str = ".mlrc-write-probe";

const MINECRAFT_DIR: &str = ".minecraft";
const LAUNCHER_DIR: &str = "MLRC";

/// Resolved layout.
///
/// Portable mode is the default: `<directory of mlrc.exe>`. It is never silently
/// replaced by a per-user location - if the directory is not writable the caller gets
/// How the base directory was chosen.
///
/// `Portable` is everything that exists today: the data lives beside the executable, or
/// wherever `--data-dir` / `MLRC_DATA_DIR` points. `Managed` is reserved for a
/// distribution that cannot write next to itself (`Program Files`, `/usr/bin`, an
/// application bundle); nothing resolves to it yet, which is why `resolve` never
/// returns it. Keeping the distinction in the type is what lets a caller ask the
/// question before there is an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataMode {
    Portable,
    Managed,
}

impl DataMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Managed => "managed",
        }
    }
}
/// `E_PORTABLE_READONLY` and decides what to do instead.
#[derive(Debug, Clone)]
pub struct DataRoot {
    base: PathBuf,
    portable: bool,
}

impl DataRoot {
    /// Resolution order: explicit flag, `MLRC_DATA_DIR`, executable directory.
    ///
    /// The base directory *contains* `.minecraft` and `MLRC`; it is not either of them.
    pub fn resolve(explicit: Option<&Path>) -> MlrcResult<Self> {
        if let Some(path) = explicit {
            return Ok(Self {
                base: absolute(path)?,
                portable: false,
            });
        }

        if let Some(value) = std::env::var_os(ENV_DATA_DIR) {
            if !value.is_empty() {
                return Ok(Self {
                    base: absolute(Path::new(&value))?,
                    portable: false,
                });
            }
        }

        let exe = std::env::current_exe().map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot locate the executable: {err}"),
            )
        })?;
        let dir = exe.parent().ok_or_else(|| {
            MlrcError::new(ErrorCode::Io, "cannot determine the executable directory")
        })?;

        Ok(Self {
            base: dir.to_path_buf(),
            portable: true,
        })
    }

    /// Where the launcher would keep its data if it were not portable.
    ///
    /// Reserved, not used yet: a distribution in `Program Files`, `/usr/bin` or an
    /// application bundle cannot write next to its executable, and the answer will be a
    /// second mode rather than silently moving the data. The paths below are the
    /// platform conventions, so the future mode has one place to look.
    pub fn managed_dir() -> Option<PathBuf> {
        #[cfg(windows)]
        {
            std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("MLRC"))
        }
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("Library/Application Support/MLRC"))
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
                })
                .map(|base| base.join("mlrc"))
        }
    }

    /// The mode this run uses: portable (the default) or managed, when one arrives.
    pub fn mode(&self) -> DataMode {
        if self.portable {
            DataMode::Portable
        } else {
            DataMode::Managed
        }
    }

    /// The directory holding `.minecraft` and `MLRC`.
    pub fn root(&self) -> &Path {
        &self.base
    }

    pub fn is_portable(&self) -> bool {
        self.portable
    }

    // --- the standard directory ------------------------------------------------

    /// Everything other tools recognise.
    pub fn minecraft_dir(&self) -> PathBuf {
        self.base.join(MINECRAFT_DIR)
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.minecraft_dir().join("versions")
    }

    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    /// Shared Maven-style library tree, exactly where the official launcher keeps it.
    pub fn libraries_dir(&self) -> PathBuf {
        self.minecraft_dir().join("libraries")
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.minecraft_dir().join("assets")
    }

    /// The version's own directory doubles as its game directory: one version, one
    /// set of saves and mods, the same isolation the imitated launcher uses.
    pub fn game_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id)
    }

    /// Launcher-private data that belongs to one version.
    pub fn state_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(LAUNCHER_DIR)
    }

    /// Kept because installer and tooling expect to find it.
    pub fn launcher_profiles(&self) -> PathBuf {
        self.minecraft_dir().join("launcher_profiles.json")
    }

    // --- the launcher's own directory ------------------------------------------

    pub fn mlrc_dir(&self) -> PathBuf {
        self.base.join(LAUNCHER_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.mlrc_dir().join("config.toml")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.mlrc_dir().join("cache").join("objects")
    }

    pub fn java_dir(&self) -> PathBuf {
        self.mlrc_dir().join("java")
    }

    /// Stored Yggdrasil session. Plaintext, like every other launcher's profile file;
    /// it lives beside the executable because the layout is portable by design.
    pub fn auth_path(&self) -> PathBuf {
        self.mlrc_dir().join("auth.json")
    }

    /// Downloaded authlib-injector jars, one per build.
    pub fn injector_dir(&self) -> PathBuf {
        self.mlrc_dir().join("injector")
    }

    pub fn jdk_dir(&self, major: u32) -> PathBuf {
        self.java_dir().join(format!("jdk-{major}"))
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.mlrc_dir().join("meta")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.mlrc_dir().join("logs")
    }

    /// Scratch space for the game's JVM.
    ///
    /// Passed as `-Djava.io.tmpdir`: some loaders write remapped jars and image
    /// caches there, and keeping it inside the installation is what makes it
    /// genuinely portable (and works on machines whose `%TEMP%` is locked down).
    pub fn tmp_dir(&self) -> PathBuf {
        self.mlrc_dir().join("tmp")
    }

    /// Create the directory skeleton. Idempotent.
    pub fn ensure(&self) -> MlrcResult<()> {
        let dirs = [
            self.base.clone(),
            self.minecraft_dir(),
            self.versions_dir(),
            self.libraries_dir(),
            self.assets_dir(),
            self.mlrc_dir(),
            self.cache_dir(),
            self.java_dir(),
            self.meta_dir(),
            self.logs_dir(),
            self.tmp_dir(),
        ];
        for dir in dirs {
            std::fs::create_dir_all(&dir).map_err(|err| self.write_error(&dir, err))?;
        }

        self.ensure_launcher_profile()?;
        Ok(())
    }

    /// A minimal `launcher_profiles.json`.
    ///
    /// The loader installers refuse to run without one, and other tools use it to
    /// recognise the folder as a launcher directory. It contains no account data.
    fn ensure_launcher_profile(&self) -> MlrcResult<()> {
        let path = self.launcher_profiles();
        if path.is_file() {
            return Ok(());
        }

        let profile = "{\n  \"profiles\": {\n    \"MLRC\": {\n      \"name\": \"MLRC\",\n      \
                       \"type\": \"custom\",\n      \"lastVersionId\": \"latest-release\"\n    }\n  },\n  \
                       \"selectedProfile\": \"MLRC\",\n  \"clientToken\": \"mlrc\"\n}\n";

        std::fs::write(&path, profile).map_err(|err| self.write_error(&path, err))
    }

    /// Prove the base is writable by creating and removing a probe file.
    pub fn probe_writable(&self) -> MlrcResult<()> {
        std::fs::create_dir_all(&self.base).map_err(|err| self.write_error(&self.base, err))?;
        let probe = self.base.join(PROBE_FILE);
        std::fs::write(&probe, b"ok").map_err(|err| self.write_error(&self.base, err))?;
        let _ = std::fs::remove_file(&probe);
        Ok(())
    }

    fn write_error(&self, path: &Path, err: std::io::Error) -> MlrcError {
        let code = if self.portable {
            ErrorCode::PortableReadOnly
        } else {
            ErrorCode::Io
        };

        let mut error = MlrcError::new(code, format!("cannot write {}: {err}", path.display()))
            .with("path", path.display().to_string());

        // The usual causes are not "the disk is full": a folder copied out of an
        // archive, a security product blocking an unsigned binary, or a read-only
        // share. Say what to try instead of leaving the user with errno 5.
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            error = error.with(
                "hint",
                "the folder is not writable by this process; move the launcher to a \
                 folder you own, pass --data-dir, or allow it in your security software",
            );
        }

        error
    }
}

fn absolute(path: &Path) -> MlrcResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot read the current directory: {err}"),
        )
    })?;
    Ok(cwd.join(path))
}
