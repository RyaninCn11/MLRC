//! Unpacking native libraries.
//!
//! Only old versions ship natives the launcher extracts itself. The directory layout
//! follows the launcher we imitate: one root per version with four areas, so the JVM
//! flags can name them separately.
//!
//! What goes where is not a matter of taste. Documents up to 1.18 declare their
//! natives through `extract`, and the game then loads those DLLs through
//! `java.library.path` - so everything the launcher unpacks must land in `java/`,
//! whatever library it came from. The `jna`, `lwjgl` and `netty` areas belong to
//! libraries that ride the classpath and unpack themselves at runtime.

use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::plan::install::InstallPlan;
use crate::port::DataRoot;

/// The four areas, and the order the flags are emitted in.
pub const CATEGORIES: [&str; 4] = ["java", "jna", "lwjgl", "netty"];

/// `<versions>/<id>/<id>-natives`, a sibling of the version's jar.
pub fn natives_root(root: &DataRoot, id: &str) -> PathBuf {
    root.version_dir(id).join(format!("{id}-natives"))
}

/// The area launcher-extracted natives go to, and the one `java.library.path` names.
pub const JAVA_AREA: &str = "java";

/// Creates the four directories, which is what the flags expect to exist.
pub fn prepare(root: &DataRoot, id: &str) -> MlrcResult<PathBuf> {
    let root_dir = natives_root(root, id);
    for category in CATEGORIES {
        std::fs::create_dir_all(root_dir.join(category))?;
    }
    Ok(root_dir)
}

/// Unpacks every native jar the launcher is responsible for.
///
/// Always into `java/`: for documents that extract natives (up to 1.18) the game
/// loads them through `java.library.path`, so splitting by library family would put
/// the DLLs somewhere that path does not cover. The family directories exist for the
/// libraries that unpack themselves at runtime.
pub fn extract(plan: &InstallPlan, root: &DataRoot, id: &str) -> MlrcResult<usize> {
    let natives_root = prepare(root, id)?;
    let dir = natives_root.join(JAVA_AREA);

    let mut extracted = 0;
    for artifact in &plan.natives {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::version::Artifact;

    /// A document that extracts natives (up to 1.18) must end up in `java/`, because
    /// that is what `java.library.path` names; putting LWJGL's DLLs anywhere else
    /// makes the game die with "Failed to load a library".
    #[test]
    fn extracted_natives_land_in_the_java_area() {
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/natives-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let root = DataRoot::resolve(Some(&base)).expect("data root");
        root.ensure().expect("ensure");

        let id = "1.18.2";
        let dir = natives_root(&root, id).join(JAVA_AREA);
        std::fs::create_dir_all(&dir).unwrap();

        // The jar's name carries `lwjgl` to prove the path is not used for sorting.
        let jar = dir.join("lwjgl-3.2.2-natives-windows.jar");
        {
            let file = std::fs::File::create(&jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("lwjgl.dll", zip::write::SimpleFileOptions::default())
                .unwrap();
            use std::io::Write;
            zip.write_all(b"not really a dll").unwrap();
            zip.finish().unwrap();
        }

        let plan = InstallPlan {
            id: id.to_string(),
            main_class: String::new(),
            java: None,
            client: None,
            libraries: Vec::new(),
            natives: vec![Artifact {
                path: "org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2-natives-windows.jar".to_string(),
                sha1: String::new(),
                size: 0,
                url: String::new(),
            }],
            asset_index: None,
            logging: None,
            bytes: 0,
            files: 0,
        };

        let extracted = extract(&plan, &root, id).expect("extract");
        assert_eq!(extracted, 1);
        assert!(dir.join("lwjgl.dll").is_file(), "the dll stays in java/");
        assert!(
            !natives_root(&root, id)
                .join("lwjgl")
                .join("lwjgl.dll")
                .exists(),
            "nothing is unpacked into the runtime areas"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
