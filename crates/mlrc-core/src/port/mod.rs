//! Where MLRC keeps its data, and how it decides that without asking anyone.

pub mod config;
pub mod state;

use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

pub const ENV_DATA_DIR: &str = "MLRC_DATA_DIR";
const PROBE_FILE: &str = ".mlrc-write-probe";

/// Resolved data root.
///
/// Portable mode is the default: `<directory of mlrc.exe>/data`. It is never
/// silently replaced by a per-user location - if the directory is not writable the
/// caller gets `E_PORTABLE_READONLY` and decides what to do instead.
#[derive(Debug, Clone)]
pub struct DataRoot {
    root: PathBuf,
    portable: bool,
}

impl DataRoot {
    /// Resolution order: explicit flag, `MLRC_DATA_DIR`, executable directory.
    pub fn resolve(explicit: Option<&Path>) -> MlrcResult<Self> {
        if let Some(path) = explicit {
            return Ok(Self {
                root: absolute(path)?,
                portable: false,
            });
        }

        if let Some(value) = std::env::var_os(ENV_DATA_DIR) {
            if !value.is_empty() {
                return Ok(Self {
                    root: absolute(Path::new(&value))?,
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
            root: dir.join("data"),
            portable: true,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_portable(&self) -> bool {
        self.portable
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    pub fn java_dir(&self) -> PathBuf {
        self.root.join("java")
    }

    pub fn jdk_dir(&self, major: u32) -> PathBuf {
        self.java_dir().join(format!("jdk-{major}"))
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache").join("objects")
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets")
    }

    pub fn minecraft_dir(&self) -> PathBuf {
        self.root.join("minecraft")
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.root.join("meta")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }

    /// Scratch space for the game's JVM.
    ///
    /// Passed as `-Djava.io.tmpdir`: some loaders write remapped jars and image
    /// caches there, and keeping it inside `data/` is what makes the installation
    /// genuinely portable (and works on machines whose `%TEMP%` is locked down).
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Create the directory skeleton. Idempotent.
    pub fn ensure(&self) -> MlrcResult<()> {
        let dirs = [
            self.root.clone(),
            self.versions_dir(),
            self.java_dir(),
            self.cache_dir(),
            self.assets_dir(),
            self.minecraft_dir(),
            self.meta_dir(),
            self.logs_dir(),
            self.tmp_dir(),
        ];
        for dir in dirs {
            std::fs::create_dir_all(&dir).map_err(|err| self.write_error(&dir, err))?;
        }
        Ok(())
    }

    /// Prove the root is writable by creating and removing a probe file.
    pub fn probe_writable(&self) -> MlrcResult<()> {
        std::fs::create_dir_all(&self.root).map_err(|err| self.write_error(&self.root, err))?;
        let probe = self.root.join(PROBE_FILE);
        std::fs::write(&probe, b"ok").map_err(|err| self.write_error(&self.root, err))?;
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
