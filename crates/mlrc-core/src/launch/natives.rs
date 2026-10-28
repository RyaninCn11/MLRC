//! Unpacking native libraries.
//!
//! Only old versions need this. Since 1.19 the native jars ride on the classpath and
//! LWJGL extracts what it needs itself, so an empty native set is normal, not a
//! failure.

use std::path::Path;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::plan::install::InstallPlan;

pub fn extract(plan: &InstallPlan, version_dir: &Path) -> MlrcResult<usize> {
    if plan.natives.is_empty() {
        return Ok(0);
    }

    let natives_dir = version_dir.join("natives");
    std::fs::create_dir_all(&natives_dir)?;

    let mut extracted = 0;
    for artifact in &plan.natives {
        let jar = natives_dir.join(artifact.file_name());
        if !jar.is_file() {
            continue;
        }
        extracted += extract_jar(&jar, &natives_dir)?;
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
