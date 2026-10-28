//! Per-version install state: the file that makes installation resumable.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Which cache objects a version (or a Java runtime) references.
///
/// `cache prune` needs this: an object is only unreferenced when no installed
/// version lists it. Reading link counts would be shorter, but the Windows API for
/// that is still unstable, and an explicit list is deterministic anyway.
pub const OBJECTS_FILE: &str = "objects.txt";

pub fn write_objects(dir: &Path, objects: &BTreeSet<String>) -> MlrcResult<()> {
    let mut text = String::with_capacity(objects.len() * 41);
    for sha1 in objects {
        text.push_str(sha1);
        text.push('\n');
    }
    std::fs::write(dir.join(OBJECTS_FILE), text).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot write the object list in {}: {err}", dir.display()),
        )
    })
}

pub fn read_objects(dir: &Path) -> MlrcResult<BTreeSet<String>> {
    match std::fs::read_to_string(dir.join(OBJECTS_FILE)) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(err) => Err(MlrcError::new(
            ErrorCode::Io,
            format!("cannot read the object list in {}: {err}", dir.display()),
        )),
    }
}

/// `versions/<id>/installed.json`.
///
/// A version is installed only when `complete` is true. An interrupted install
/// leaves `complete: false` behind, which `install` repairs and `launch` refuses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledState {
    pub id: String,
    pub complete: bool,
    pub java_component: Option<String>,
    pub java_major: Option<u32>,
    pub files: u32,
    pub bytes: u64,
    pub installed_at: String,
}

impl InstalledState {
    pub const FILE_NAME: &'static str = "installed.json";

    pub fn read(path: &Path) -> MlrcResult<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let state = serde_json::from_str(&text).map_err(|err| {
                    MlrcError::new(
                        ErrorCode::MetaShape,
                        format!("invalid {}: {err}", path.display()),
                    )
                    .with("path", path.display().to_string())
                })?;
                Ok(Some(state))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    pub fn write(&self, path: &Path) -> MlrcResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot create {}: {err}", parent.display()),
                )
            })?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))?;
        std::fs::write(path, text).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot write {}: {err}", path.display()),
            )
        })
    }
}
