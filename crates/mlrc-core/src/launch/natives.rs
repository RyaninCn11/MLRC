//! Unpacking native libraries.
//!
//! Only old versions ship natives the launcher extracts itself. The directory layout
//! follows the launcher we imitate: one root per version with four subdirectories, so
//! each library family keeps its own extraction area and the JVM flags can point at
//! them separately.

use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::meta::version::Artifact;
use crate::plan::install::InstallPlan;
use crate::port::DataRoot;

/// The four areas, and the order the flags are emitted in.
pub const CATEGORIES: [&str; 4] = ["java", "jna", "lwjgl", "netty"];

/// `<versions>/<id>/<id>-natives`, a sibling of the version's jar.
pub fn natives_root(root: &DataRoot, id: &str) -> PathBuf {
    root.version_dir(id).join(format!("{id}-natives"))
}

/// Which subdirectory a native jar belongs to.
///
/// Classified by the Maven path, which is the only stable identity an artifact has
/// after the document has been merged.
pub fn category(artifact: &Artifact) -> &'static str {
    let path = artifact.path.to_ascii_lowercase();
    if path.contains("jna") {
        "jna"
    } else if path.contains("lwjgl") {
        "lwjgl"
    } else if path.contains("netty") {
        "netty"
    } else {
        "java"
    }
}

/// Creates the four directories, which is what the flags expect to exist.
pub fn prepare(root: &DataRoot, id: &str) -> MlrcResult<PathBuf> {
    let root_dir = natives_root(root, id);
    for category in CATEGORIES {
        std::fs::create_dir_all(root_dir.join(category))?;
    }
    Ok(root_dir)
}

/// Unpacks every native jar into its area.
pub fn extract(plan: &InstallPlan, root: &DataRoot, id: &str) -> MlrcResult<usize> {
    let natives_root = prepare(root, id)?;

    let mut extracted = 0;
    for artifact in &plan.natives {
        let dir = natives_root.join(category(artifact));
        let jar = dir.join(artifact.file_name());
        if !jar.is_file() {
            continue;
        }
        extracted += extract_jar(&jar, &dir)?;
    }
    Ok(extracted)
}

fn extract_jar(jar: &Path, dest_dir: &Path) -> MlrcResult<usize> {
    let file = std::fs::File::open(jar).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot open {}: {err}", jar.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot read native jar {}: {err}", jar.display()),
        )
    })?;

    let mut extracted = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot read an entry of {}: {err}", jar.display()),
            )
        })?;

        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name.starts_with("META-INF/") || !is_native(&name) {
            continue;
        }
        let Some(file_name) = Path::new(&name).file_name() else {
            continue;
        };
        let target = dest_dir.join(file_name);
        if target.exists() {
            continue;
        }

        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        extracted += 1;
    }

    Ok(extracted)
}

fn is_native(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".dll", ".so", ".dylib", ".jnilib"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}
