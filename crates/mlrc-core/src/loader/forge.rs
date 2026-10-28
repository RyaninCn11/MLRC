//! Forge and NeoForge.
//!
//! Both ship an installer jar that patches the vanilla client and writes a version
//! document. MLRC does not reimplement their processors; it runs the official
//! installer against a staging launcher directory and then adopts what it produced.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::EventSink;
use crate::fetch::{HttpClient, Mirror};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Forge,
    NeoForge,
}

impl Flavour {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "forge" => Some(Self::Forge),
            "neoforge" => Some(Self::NeoForge),
            _ => None,
        }
    }

    const fn metadata_url(self) -> &'static str {
        match self {
            Self::Forge => {
                "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml"
            }
            Self::NeoForge => {
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml"
            }
        }
    }

    fn installer_url(self, version: &str) -> String {
        match self {
            Self::Forge => format!(
                "https://maven.minecraftforge.net/net/minecraftforge/forge/{version}/forge-{version}-installer.jar"
            ),
            Self::NeoForge => format!(
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar"
            ),
        }
    }

    /// The id the installer writes into `versions/`.
    ///
    /// Forge publishes `1.20.1-47.4.28` but names the installed profile
    /// `1.20.1-forge-47.4.28`; NeoForge's own version already is the id.
    fn id(self, mc: &str, version: &str) -> String {
        match self {
            Self::Forge => {
                let build = version.strip_prefix(&format!("{mc}-")).unwrap_or(version);
                format!("{mc}-forge-{build}")
            }
            Self::NeoForge => format!("neoforge-{version}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Installer {
    pub flavour: Flavour,
    pub version: String,
    pub url: String,
    pub id: String,
}

pub async fn resolve(
    client: &HttpClient,
    flavour: Flavour,
    mc: &str,
    requested: Option<&str>,
) -> MlrcResult<Installer> {
    let xml = fetch_metadata(client, flavour).await?;
    let xml = String::from_utf8_lossy(&xml);
    let versions = parse_versions(&xml);

    let prefix = match flavour {
        Flavour::Forge => format!("{mc}-"),
        Flavour::NeoForge => neoforge_prefix(mc).ok_or_else(|| {
            MlrcError::new(
                ErrorCode::LoaderUnavailable,
                format!("neoforge does not support minecraft {mc}"),
            )
            .with("minecraft", mc.to_string())
        })?,
    };

    let candidates: Vec<&String> = versions
        .iter()
        .filter(|version| version.starts_with(&prefix))
        .collect();

    let version = match requested {
        Some(wanted) => {
            let full = format!("{prefix}{wanted}");
            candidates
                .iter()
                .find(|version| version.as_str() == wanted || version.as_str() == full)
                .map(|version| (*version).clone())
                .ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::LoaderUnavailable,
                        format!(
                            "{} has no version {wanted} for minecraft {mc}",
                            flavour.as_str()
                        ),
                    )
                })?
        }
        None => candidates
            .into_iter()
            .max_by(|left, right| compare_versions(left, right))
            .cloned()
            .ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::LoaderUnavailable,
                    format!("{} has no installer for minecraft {mc}", flavour.as_str()),
                )
                .with("minecraft", mc.to_string())
            })?,
    };

    Ok(Installer {
        url: flavour.installer_url(&version),
        id: flavour.id(mc, &version),
        flavour,
        version,
    })
}

/// NeoForge numbers its releases after the Minecraft version: `1.21.1` becomes
/// `21.1.x`, and the scheme only exists from 1.20.2 onwards.
fn neoforge_prefix(mc: &str) -> Option<String> {
    let parts: Vec<&str> = mc.split('.').collect();
    if parts.len() < 3 || parts[0] != "1" {
        return None;
    }
    let major: u32 = parts[1].parse().ok()?;
    let minor: u32 = parts[2].parse().ok()?;
    if major < 20 || (major == 20 && minor < 2) {
        return None;
    }
    Some(format!("{major}.{minor}."))
}

/// Metadata is small and must not hang: a maven host that accepts the connection and
/// then goes quiet would otherwise hold the command for the full client timeout.
/// Pre-downloads everything the installer would fetch from its own maven.
///
/// NeoForge's installer ignores `--mirror` for library downloads, so on a network
/// where `maven.neoforged.net` is unreachable the install can never finish. The
/// installer skips files that already exist, which makes seeding it ourselves the
/// one reliable way to route those downloads through the configured mirror.
pub async fn preseed_libraries(
    client: &HttpClient,
    stage: &Path,
    installer_jar: &Path,
    sink: &dyn EventSink,
) -> MlrcResult<usize> {
    let artifacts = read_install_manifest(installer_jar)?;
    if artifacts.is_empty() {
        return Ok(0);
    }

    sink.emit(crate::events::Event::Note {
        message: format!("pre-downloading {} installer libraries", artifacts.len()),
    });

    let mut downloaded = 0;
    for (path, url, sha1) in artifacts {
        let target = stage.join("libraries").join(&path);
        if target.is_file() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let bytes = match installer_bytes(client, &url).await {
            Ok(bytes) => bytes,
            // A missing optional artifact must not abort: the installer decides.
            Err(_) => continue,
        };

        if !sha1.is_empty() {
            let actual = sha1_of(&bytes);
            if actual != sha1 {
                return Err(MlrcError::new(
                    ErrorCode::HashMismatch,
                    format!("hash mismatch for installer library {}", path.display()),
                )
                .with("expected", sha1)
                .with("actual", actual));
            }
        }

        std::fs::write(&target, bytes)?;
        downloaded += 1;
    }

    Ok(downloaded)
}

/// `(path, url, sha1)` for every library either manifest mentions.
fn read_install_manifest(jar: &Path) -> MlrcResult<Vec<(PathBuf, String, String)>> {
    let file = std::fs::File::open(jar).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot open {}: {err}", jar.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot read installer {}: {err}", jar.display()),
        )
    })?;

    let mut seen = BTreeSet::new();
    let mut artifacts = Vec::new();

    for name in ["install_profile.json", "version.json"] {
        let Ok(mut entry) = archive.by_name(name) else {
            continue;
        };
        let mut text = String::new();
        use std::io::Read;
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        drop(entry);

        let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(libraries) = document.get("libraries").and_then(|value| value.as_array()) else {
            continue;
        };

        for library in libraries {
            let Some((path, url, sha1)) = library_artifact(library) else {
                continue;
            };
            if seen.insert(path.clone()) {
                artifacts.push((PathBuf::from(path), url, sha1));
            }
        }
    }

    Ok(artifacts)
}

fn library_artifact(library: &serde_json::Value) -> Option<(String, String, String)> {
    if let Some(artifact) = library.pointer("/downloads/artifact") {
        let path = artifact.get("path")?.as_str()?.to_string();
        let url = artifact.get("url")?.as_str()?.to_string();
        let sha1 = artifact
            .get("sha1")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        return Some((path, url, sha1));
    }

    // Maven-coordinate form: `name` plus a base `url`.
    let base = library.get("url")?.as_str()?;
    let name = library.get("name")?.as_str()?;
    let path = maven_path(name)?;
    Some((path.clone(), format!("{base}{path}"), String::new()))
}

fn maven_path(coordinate: &str) -> Option<String> {
    let mut parts = coordinate.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();
    let file = match classifier {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!(
        "{}/{artifact}/{version}/{file}",
        group.replace('.', "/")
    ))
}

fn sha1_of(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn fetch_metadata(client: &HttpClient, flavour: Flavour) -> MlrcResult<Vec<u8>> {
    let url = flavour.metadata_url();
    match client
        .get_bytes_with_timeout(url, Duration::from_secs(10))
        .await
    {
        Ok(bytes) => Ok(bytes),
        Err(first) => match fetch_mirrored(client, url, Duration::from_secs(10)).await {
            Ok(bytes) => Ok(bytes),
            Err(_) => Err(first),
        },
    }
}

/// The installer jar is 8-20 MB, so a longer budget, but the same fallback: several
/// networks reach one loader's maven and not the other's.
pub async fn installer_bytes(client: &HttpClient, url: &str) -> MlrcResult<Vec<u8>> {
    match client
        .get_bytes_with_timeout(url, Duration::from_secs(90))
        .await
    {
        Ok(bytes) => Ok(bytes),
        Err(first) => match fetch_mirrored(client, url, Duration::from_secs(90)).await {
            Ok(bytes) => Ok(bytes),
            Err(_) => Err(first),
        },
    }
}

async fn fetch_mirrored(client: &HttpClient, url: &str, timeout: Duration) -> MlrcResult<Vec<u8>> {
    let mirrored = Mirror::Bmclapi.rewrite(url);
    if mirrored == url {
        return Err(MlrcError::new(
            ErrorCode::Network,
            format!("no mirror is known for {url}"),
        ));
    }
    client.get_bytes_with_timeout(&mirrored, timeout).await
}

fn parse_versions(xml: &str) -> Vec<String> {
    let mut versions = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<version>") {
        let after = &rest[start + "<version>".len()..];
        let Some(end) = after.find("</version>") else {
            break;
        };
        versions.push(after[..end].trim().to_string());
        rest = &after[end + "</version>".len()..];
    }

    versions
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let numbers = |text: &str| -> Vec<u64> {
        text.split(['.', '-'])
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    numbers(left).cmp(&numbers(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_extracted_from_metadata() {
        let xml = "<metadata><versioning><versions><version>1.20.1-47.2.0</version>\
                   <version>1.20.1-47.4.28</version></versions></versioning></metadata>";
        assert_eq!(
            parse_versions(xml),
            vec!["1.20.1-47.2.0".to_string(), "1.20.1-47.4.28".to_string()]
        );
    }

    #[test]
    fn neoforge_prefix_follows_the_minecraft_version() {
        assert_eq!(neoforge_prefix("1.21.1").as_deref(), Some("21.1."));
        assert_eq!(neoforge_prefix("1.20.2").as_deref(), Some("20.2."));
        assert_eq!(neoforge_prefix("1.20.1"), None);
        assert_eq!(neoforge_prefix("1.12.2"), None);
    }

    #[test]
    fn version_order_is_numeric() {
        assert_eq!(
            compare_versions("1.20.1-47.4.28", "1.20.1-47.2.0"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn ids_match_what_the_installers_write() {
        assert_eq!(
            Flavour::Forge.id("1.20.1", "1.20.1-47.4.28"),
            "1.20.1-forge-47.4.28"
        );
        assert_eq!(
            Flavour::NeoForge.id("1.21.1", "21.1.80"),
            "neoforge-21.1.80"
        );
    }
}
