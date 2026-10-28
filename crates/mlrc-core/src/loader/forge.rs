//! Forge and NeoForge.
//!
//! Both ship an installer jar that patches the vanilla client and writes a version
//! document. MLRC does not reimplement their processors; it runs the official
//! installer against a staging launcher directory and then adopts what it produced.

use std::time::Duration;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
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
