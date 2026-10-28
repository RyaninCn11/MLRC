//! Download sources.
//!
//! A mirror is pure URL substitution: the hash in the version document is still the
//! authority, so a mirror can be slow, wrong or hostile without corrupting an
//! install - the SHA1 check catches it.
//!
//! BMCLAPI maps official hosts onto `bmclapi2.bangbang93.com`, which then redirects
//! to a node. Paths are preserved, which is why a prefix table is enough:
//!
//! | official | BMCLAPI |
//! |---|---|
//! | `piston-meta.mojang.com` | `bmclapi2.bangbang93.com` |
//! | `piston-data.mojang.com` | `bmclapi2.bangbang93.com` |
//! | `launchermeta.mojang.com` | `bmclapi2.bangbang93.com` |
//! | `libraries.minecraft.net` | `bmclapi2.bangbang93.com/maven` |
//! | `resources.download.minecraft.net` | `bmclapi2.bangbang93.com/assets` |
//! | `meta.fabricmc.net` | `bmclapi2.bangbang93.com/fabric-meta` |
//! | `maven.fabricmc.net` | `bmclapi2.bangbang93.com/maven` |

use std::time::{Duration, Instant};

use crate::port::config;

pub const BMCLAPI_BASE: &str = "https://bmclapi2.bangbang93.com";

/// Effective source. `Auto` is a preference, not a source, so it is resolved before
/// a client is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    Official,
    Bmclapi,
}

const REPLACEMENTS: [(&str, &str); 10] = [
    ("https://piston-meta.mojang.com", BMCLAPI_BASE),
    ("https://piston-data.mojang.com", BMCLAPI_BASE),
    ("https://launchermeta.mojang.com", BMCLAPI_BASE),
    ("https://launcher.mojang.com", BMCLAPI_BASE),
    (
        "https://libraries.minecraft.net",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    (
        "https://resources.download.minecraft.net",
        "https://bmclapi2.bangbang93.com/assets",
    ),
    (
        "https://meta.fabricmc.net",
        "https://bmclapi2.bangbang93.com/fabric-meta",
    ),
    (
        "https://maven.fabricmc.net",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    (
        "https://maven.minecraftforge.net",
        "https://bmclapi2.bangbang93.com/maven",
    ),
    (
        "https://maven.neoforged.net/releases",
        "https://bmclapi2.bangbang93.com/maven",
    ),
];

impl Mirror {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Bmclapi => "bmclapi",
        }
    }

    pub fn rewrite(self, url: &str) -> String {
        if self == Self::Official {
            return url.to_string();
        }
        for (from, to) in REPLACEMENTS {
            if let Some(rest) = url.strip_prefix(from) {
                return format!("{to}{rest}");
            }
        }
        url.to_string()
    }

    pub const fn to_config(self) -> config::Mirror {
        match self {
            Self::Official => config::Mirror::Official,
            Self::Bmclapi => config::Mirror::Bmclapi,
        }
    }

    pub fn from_config(mirror: config::Mirror) -> Option<Self> {
        match mirror {
            config::Mirror::Auto => None,
            config::Mirror::Official => Some(Self::Official),
            config::Mirror::Bmclapi => Some(Self::Bmclapi),
        }
    }
}

/// Time both mirrors and keep the faster one. Never fails: on a total network
/// outage it reports `Official` and lets the download produce the real error.
pub async fn probe() -> Mirror {
    let client = match reqwest::Client::builder()
        .user_agent(format!("mlrc/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Mirror::Official,
    };

    let official = measure(
        &client,
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
    )
    .await;
    let bmclapi = measure(
        &client,
        "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json",
    )
    .await;

    match (official, bmclapi) {
        (Some(official), Some(bmclapi)) => {
            if bmclapi < official {
                Mirror::Bmclapi
            } else {
                Mirror::Official
            }
        }
        (Some(_), None) => Mirror::Official,
        (None, Some(_)) => Mirror::Bmclapi,
        (None, None) => Mirror::Official,
    }
}

async fn measure(client: &reqwest::Client, url: &str) -> Option<Duration> {
    let started = Instant::now();
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    // Headers are all that matter for latency; the body is dropped unread.
    Some(started.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_urls_are_untouched_by_the_official_mirror() {
        let url = "https://piston-meta.mojang.com/v1/packages/abc/1.21.1.json";
        assert_eq!(Mirror::Official.rewrite(url), url);
    }

    #[test]
    fn piston_hosts_collapse_onto_bmclapi() {
        assert_eq!(
            Mirror::Bmclapi
                .rewrite("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"),
            "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://piston-data.mojang.com/v1/objects/aa/client.jar"),
            "https://bmclapi2.bangbang93.com/v1/objects/aa/client.jar"
        );
    }

    #[test]
    fn maven_and_assets_get_their_own_prefix() {
        assert_eq!(
            Mirror::Bmclapi
                .rewrite("https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar"),
            "https://bmclapi2.bangbang93.com/maven/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://resources.download.minecraft.net/f0/f00657542252858a721e715a2e888a9226404e35"),
            "https://bmclapi2.bangbang93.com/assets/f0/f00657542252858a721e715a2e888a9226404e35"
        );
    }

    #[test]
    fn fabric_hosts_are_mirrored() {
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://meta.fabricmc.net/v2/versions/loader/1.21.1"),
            "https://bmclapi2.bangbang93.com/fabric-meta/v2/versions/loader/1.21.1"
        );
        assert_eq!(
            Mirror::Bmclapi.rewrite("https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"),
            "https://bmclapi2.bangbang93.com/maven/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
        );
    }

    #[test]
    fn unknown_hosts_pass_through() {
        let url = "https://api.minecraftservices.com/minecraft/profile";
        assert_eq!(Mirror::Bmclapi.rewrite(url), url);
    }
}
