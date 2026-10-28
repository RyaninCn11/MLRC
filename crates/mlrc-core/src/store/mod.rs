//! Content-addressed object store.
//!
//! Every file is stored once under `cache/objects/<aa>/<sha1>` and then hardlinked
//! into the version directory. Installing 1.18.2 and 1.19 therefore does not store
//! Guava twice - it stores one inode and two directory entries.

use std::path::{Path, PathBuf};

use crate::error::MlrcResult;
use crate::port::DataRoot;

pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: &DataRoot) -> Self {
        Self {
            root: root.cache_dir(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `objects/<first two>/<full sha1>` - the same shape the official launcher
    /// uses for assets, so the store and `assets/objects` stay recognisable.
    pub fn path_for(&self, sha1: &str) -> PathBuf {
        let prefix = &sha1[..sha1.len().min(2)];
        self.root.join(prefix).join(sha1)
    }

    /// Size check only: cheap, and a wrong size is the common failure anyway.
    /// Hash verification happens when the file is downloaded. Zero-byte files exist
    /// in Mojang's manifests, so they are matched on existence alone.
    pub fn contains(&self, sha1: &str, size: u64) -> bool {
        match std::fs::metadata(self.path_for(sha1)) {
            Ok(metadata) => size == 0 || metadata.len() == size,
            Err(_) => false,
        }
    }

    /// Hardlink when the destination shares a volume, copy otherwise.
    pub fn materialize(&self, sha1: &str, dest: &Path) -> MlrcResult<()> {
        if dest.exists() {
            return Ok(());
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let source = self.path_for(sha1);
        match std::fs::hard_link(&source, dest) {
            Ok(()) => Ok(()),
            Err(_) => {
                std::fs::copy(&source, dest)?;
                Ok(())
            }
        }
    }
}
