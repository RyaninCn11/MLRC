//! The version document. Every field defaults: Mojang adds fields between releases
//! and a launcher that panics on the unknown is a broken launcher.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;
use crate::meta::rule::Rule;
use crate::port::DataRoot;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub main_class: String,
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// Pre-1.13 style launch arguments. Kept so old documents parse, not yet used.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub downloads: Downloads,
    #[serde(default)]
    pub asset_index: Option<AssetIndex>,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<Logging>,
    #[serde(default)]
    pub release_time: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub compliance_level: Option<u32>,
    #[serde(default)]
    pub minimum_launcher_version: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Arguments {
    pub game: Vec<Argument>,
    pub jvm: Vec<Argument>,
    /// Present since 2025; used when the user has not set JVM flags of their own.
    /// Note the kebab-case key - `rename_all = "camelCase"` does not cover it.
    #[serde(rename = "default-user-jvm")]
    pub default_user_jvm: Vec<Argument>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Conditional {
        /// Absent means unconditional. `default-user-jvm` relies on this: its first
        /// entry is a bare `{"value": [...]}` with no rules at all.
        #[serde(default)]
        rules: Vec<Rule>,
        value: StringList,
    },
}

impl Argument {
    /// Append this argument's values, if its rules match.
    pub fn collect(&self, env: &crate::meta::rule::Env, out: &mut Vec<String>) {
        match self {
            Self::Plain(value) => out.push(value.clone()),
            Self::Conditional { rules, value } => {
                if crate::meta::rule::allowed(rules, env) {
                    value.collect(out);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringList {
    One(String),
    Many(Vec<String>),
}

impl StringList {
    pub fn collect(&self, out: &mut Vec<String>) {
        match self {
            Self::One(value) => out.push(value.clone()),
            Self::Many(values) => out.extend(values.iter().cloned()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: LibraryDownloads,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub natives: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub extract: Option<Extract>,
    /// Maven base URL, used by libraries that publish coordinates instead of a
    /// `downloads` block (Fabric's loader libraries, among others).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
}

impl Library {
    /// Converts a Maven coordinate plus base URL into the same shape as a
    /// `downloads.artifact`, so the rest of the pipeline does not care which form a
    /// document used.
    pub fn maven_artifact(&self) -> Option<Artifact> {
        let base = self.url.as_deref()?;
        let path = maven_path(&self.name)?;
        Some(Artifact {
            url: format!("{base}{path}"),
            path,
            sha1: self.sha1.clone(),
            size: self.size,
        })
    }
}

/// `group:artifact:version[:classifier]` -> a repository path.
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<BTreeMap<String, Artifact>>,
}

impl LibraryDownloads {
    pub fn classifier(&self, name: &str) -> Option<&Artifact> {
        self.classifiers.as_ref().and_then(|map| map.get(name))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
}

impl Artifact {
    /// Some artifacts (the log4j config is the important one) carry no `path`, so
    /// the name falls back to the last URL segment. Install and launch both use
    /// this, which is what keeps them agreeing on where the file lives.
    pub fn file_name(&self) -> String {
        if !self.path.is_empty() {
            if let Some(name) = std::path::Path::new(&self.path).file_name() {
                return name.to_string_lossy().to_string();
            }
        }
        if let Some(name) = self.url.rsplit('/').next() {
            if !name.is_empty() && !name.contains('?') {
                return name.to_string();
            }
        }
        "artifact.jar".to_string()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Downloads {
    #[serde(default)]
    pub client: Option<Artifact>,
    #[serde(default)]
    pub server: Option<Artifact>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndex {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub total_size: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub major_version: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logging {
    #[serde(default)]
    pub client: Option<LoggingClient>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingClient {
    #[serde(default)]
    pub argument: String,
    #[serde(default)]
    pub file: Artifact,
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl VersionJson {
    /// `<.minecraft>/versions/<id>/<id>.json`, the standard location.
    ///
    /// Other launchers and the loader installers look here, and it keeps a version
    /// self-contained: deleting the directory removes the document with it.
    pub fn cache_path(root: &DataRoot, id: &str) -> PathBuf {
        root.version_dir(id).join(format!("{id}.json"))
    }

    /// Reads the copy written by a previous install, without touching the network.
    pub fn read_cached(root: &DataRoot, id: &str) -> MlrcResult<Option<Self>> {
        let path = Self::cache_path(root, id);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", path.display()),
            )),
        }
    }

    /// Content-addressed on Mojang's side, so a cached copy never goes stale.
    pub async fn load(
        client: &HttpClient,
        root: &DataRoot,
        id: &str,
        url: &str,
    ) -> MlrcResult<Self> {
        let path = Self::cache_path(root, id);

        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(version) = serde_json::from_slice::<Self>(&bytes) {
                return Ok(version);
            }
        }

        let bytes = client.get_bytes(url).await?;
        let version: Self = serde_json::from_slice(&bytes).map_err(|err| {
            MlrcError::new(
                ErrorCode::MetaShape,
                format!("cannot parse version {id}: {err}"),
            )
            .with("version", id)
        })?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &bytes)?;

        Ok(version)
    }

    /// Stores a document that did not come from Mojang (a loader profile), so that
    /// launch can later find it in the same place as a vanilla document.
    pub fn store_cached(root: &DataRoot, id: &str, bytes: &[u8]) -> MlrcResult<()> {
        let path = Self::cache_path(root, id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot write {}: {err}", path.display()),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::rule::Env;

    /// 26.x ships `default-user-jvm`, a kebab-case key whose first entry has no
    /// `rules` at all. Both details silently produced an empty list before.
    #[test]
    fn kebab_case_default_user_jvm_parses_and_collects() {
        let json = r#"{"id":"26.3","arguments":{"default-user-jvm":[
            {"value":["-Xms2G","-Xmx4G"]},
            {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":["-XX:+UseZGC"]}
        ]}}"#;

        let version: VersionJson = serde_json::from_str(json).expect("parses");
        let arguments = version.arguments.expect("arguments present");
        assert_eq!(arguments.default_user_jvm.len(), 2);

        let env = Env::detect();
        let mut collected = Vec::new();
        for argument in &arguments.default_user_jvm {
            argument.collect(&env, &mut collected);
        }

        assert!(collected.contains(&"-Xms2G".to_string()));
        let zgc = collected.iter().any(|arg| arg == "-XX:+UseZGC");
        assert_eq!(zgc, cfg!(windows));
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let json = r#"{"id":"1.0","mainClass":"a.B","somethingNew":{"x":1},"libraries":[]}"#;
        let version: VersionJson = serde_json::from_str(json).expect("parses");
        assert_eq!(version.main_class, "a.B");
    }

    #[test]
    fn maven_coordinates_become_repository_paths() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.20.1").as_deref(),
            Some("net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar")
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.1:natives-windows").as_deref(),
            Some("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar")
        );
        assert_eq!(maven_path("broken"), None);
    }

    #[test]
    fn loader_libraries_without_downloads_get_an_artifact() {
        let json = r#"{"name":"net.fabricmc:fabric-loader:0.20.1",
            "url":"https://maven.fabricmc.net/",
            "sha1":"abc123","size":42}"#;
        let library: Library = serde_json::from_str(json).expect("library parses");

        let artifact = library.maven_artifact().expect("artifact");
        assert_eq!(
            artifact.url,
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar"
        );
        assert_eq!(artifact.sha1, "abc123");
        assert_eq!(artifact.size, 42);
    }
}
