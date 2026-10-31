//! The Modrinth API, as much of it as a launcher needs.
//!
//! Three endpoints: search, a project's versions filtered by game version and
//! loader, and a version by id (used to resolve dependencies).

use serde::Deserialize;

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::fetch::HttpClient;

const BASE: &str = "https://api.modrinth.com/v2";

/// Modrinth asks for a descriptive agent so abuse can be traced; an empty one may
/// be rate limited.
const USER_AGENT: &str = "mlrc/minecraft-launcher (github.com/mlrc-launcher)";

#[derive(Debug, Clone, Deserialize)]
pub struct Hit {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version_number: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    /// `release`, `beta` or `alpha`.
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub files: Vec<File>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct File {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub hashes: Hashes,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Hashes {
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub sha512: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    /// `required`, `optional`, `incompatible` or `embedded`.
    #[serde(default, rename = "dependency_type")]
    pub kind: String,
}

impl Version {
    /// The jar to download: the primary file, falling back to the first one.
    pub fn primary_file(&self) -> MlrcResult<&File> {
        self.files
            .iter()
            .find(|file| file.primary)
            .or_else(|| self.files.first())
            .ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::MetaShape,
                    format!("version {} has no files", self.id),
                )
            })
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    hits: Vec<Hit>,
}

pub async fn search(
    http: &HttpClient,
    query: &str,
    mc: &str,
    loader: Option<&str>,
    kind: crate::mods::Kind,
    limit: u32,
) -> MlrcResult<Vec<Hit>> {
    // Facets are AND-ed groups of OR-ed values, as JSON inside one query parameter.
    // A resource pack has no loader, so its facet list is one group shorter.
    let facets = match loader {
        Some(loader) => format!(
            r#"[["project_type:{}"],["versions:{mc}"],["categories:{loader}"]]"#,
            kind.project_type()
        ),
        None => format!(
            r#"[["project_type:{}"],["versions:{mc}"]]"#,
            kind.project_type()
        ),
    };

    let response: SearchResponse = http
        .get_json_query(
            &format!("{BASE}/search"),
            &[
                ("query", query.to_string()),
                ("facets", facets),
                ("limit", limit.to_string()),
                ("index", "relevance".to_string()),
            ],
        )
        .await?;

    Ok(response.hits)
}

/// Newest version of `project` that runs on `mc`, optionally on `loader`.
pub async fn version_for(
    http: &HttpClient,
    project: &str,
    mc: &str,
    loader: Option<&str>,
) -> MlrcResult<Version> {
    let mut query: Vec<(&str, String)> = vec![("game_versions", format!(r#"["{mc}"]"#))];
    let loader = loader.filter(|value| !value.is_empty());
    if let Some(loader) = loader {
        query.push(("loaders", format!(r#"["{loader}"]"#)));
    }

    let versions: Vec<Version> = http
        .get_json_query(&format!("{BASE}/project/{project}/version"), &query)
        .await?;

    let chosen = choose(&versions, loader.is_none());

    versions.into_iter().nth(chosen).ok_or_else(|| {
        let target = match loader {
            Some(loader) => format!("minecraft {mc} on {loader}"),
            None => format!("minecraft {mc}"),
        };
        MlrcError::new(
            ErrorCode::ModNotFound,
            format!("{project} has no release for {target}"),
        )
        .with("project", project.to_string())
    })
}

/// Which of the project's versions to take.
///
/// Modrinth returns them newest first, beta releases included, so a stable one wins a
/// tie: `mod install iris` must not quietly pick a beta.
///
/// Without a loader to filter on - a resource pack, shader or data pack - the list also
/// contains builds published for a specific mod loader, and those are the wrong file for
/// a pack folder: Terralith ships `Terralith_1.21.1_v2.6.2_Neoforge.jar` and
/// `Terralith_1.21_v2.5.8.zip` for the same game version, tagged `neoforge` and
/// `datapack`. A build that names no mod loader is preferred, and the release preference
/// still decides within each group.
fn choose(versions: &[Version], prefer_plain: bool) -> usize {
    if versions.is_empty() {
        return 0;
    }

    let mut order: Vec<usize> = (0..versions.len()).collect();
    order.sort_by_key(|index| {
        let version = &versions[*index];
        let release = version.version_type == "release";
        if !prefer_plain {
            return (!release, false);
        }

        let plain = !version.loaders.iter().any(|loader| is_mod_loader(loader));
        (!release, !plain)
    });

    order.first().copied().unwrap_or(0)
}

/// The four loaders that swap mods at startup. A build tagged with one of them is a mod
/// build, whatever else it also carries.
fn is_mod_loader(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "fabric" | "quilt" | "forge" | "neoforge"
    )
}

pub async fn version_by_id(http: &HttpClient, id: &str) -> MlrcResult<Version> {
    http.get_json(&format!("{BASE}/version/{id}")).await
}

#[derive(Deserialize)]
struct Project {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    project_type: String,
}

/// Slug for a project id, for labelling dependencies the user did not name.
/// What a project is: `mod`, `resourcepack`, `shader` or `datapack`.
///
/// `pack install` uses it to decide the destination instead of trusting a flag, so
/// `mlrc pack install sodium` fails with "that is a mod" rather than dropping a jar in
/// `resourcepacks/`.
pub async fn project_type(http: &HttpClient, id: &str) -> MlrcResult<String> {
    let project: Project = http
        .get_json_query(&format!("{BASE}/project/{id}"), &[])
        .await?;
    Ok(project.project_type)
}

pub async fn project_slug(http: &HttpClient, id: &str) -> MlrcResult<String> {
    let project: Project = http
        .get_json_query(&format!("{BASE}/project/{id}"), &[])
        .await?;
    Ok(project.slug)
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(id: &str, kind: &str, loaders: &[&str]) -> Version {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": "p",
            "version_type": kind,
            "loaders": loaders,
        }))
        .expect("version parses")
    }

    #[test]
    fn a_stable_release_beats_the_newest_beta() {
        let versions = vec![
            version("beta", "beta", &["fabric"]),
            version("stable", "release", &["fabric"]),
        ];
        assert_eq!(choose(&versions, false), 1);
    }

    #[test]
    fn a_plain_build_beats_a_loader_specific_one() {
        // Newest first, as the API returns them: the NeoForge build is newer.
        let versions = vec![
            version("neoforge", "release", &["neoforge"]),
            version("plain", "release", &["minecraft"]),
        ];
        assert_eq!(choose(&versions, true), 1, "the plain data pack wins");

        // With a loader filter there is nothing to prefer, and order decides.
        assert_eq!(choose(&versions, false), 0);
    }

    #[test]
    fn a_data_pack_beats_the_mod_build_of_the_same_project() {
        // What Terralith actually publishes for 1.21.1, newest first.
        let versions = vec![
            version("neoforge", "release", &["neoforge"]),
            version("fabric", "release", &["fabric"]),
            version("datapack", "release", &["datapack"]),
        ];
        assert_eq!(choose(&versions, true), 2, "the .zip data pack wins");
    }

    #[test]
    fn a_shader_keeps_its_loader_specific_build() {
        // Shaders are tagged iris/optifine; none is "plain", so the newest release wins.
        let versions = vec![
            version("new", "release", &["iris"]),
            version("old", "release", &["optifine"]),
        ];
        assert_eq!(choose(&versions, true), 0);
    }

    #[test]
    fn no_versions_is_not_a_panic() {
        assert_eq!(choose(&[], true), 0);
    }
}
