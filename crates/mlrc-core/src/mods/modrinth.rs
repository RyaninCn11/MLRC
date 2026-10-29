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
    loader: &str,
    limit: u32,
) -> MlrcResult<Vec<Hit>> {
    // Facets are AND-ed groups of OR-ed values, as JSON inside one query parameter.
    let facets = format!(r#"[["project_type:mod"],["versions:{mc}"],["categories:{loader}"]]"#);

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

/// Newest version of `project` that runs on `mc` with `loader`.
pub async fn version_for(
    http: &HttpClient,
    project: &str,
    mc: &str,
    loader: &str,
) -> MlrcResult<Version> {
    let versions: Vec<Version> = http
        .get_json_query(
            &format!("{BASE}/project/{project}/version"),
            &[
                ("game_versions", format!(r#"["{mc}"]"#)),
                ("loaders", format!(r#"["{loader}"]"#)),
            ],
        )
        .await?;

    // Modrinth returns newest first, beta releases included. Prefer a stable one so
    // `mod install iris` does not quietly pick a beta, but fall back rather than fail.
    let chosen = versions
        .iter()
        .position(|version| version.version_type == "release")
        .unwrap_or(0);

    versions.into_iter().nth(chosen).ok_or_else(|| {
        MlrcError::new(
            ErrorCode::ModNotFound,
            format!("{project} has no release for minecraft {mc} on {loader}"),
        )
        .with("project", project.to_string())
    })
}

pub async fn version_by_id(http: &HttpClient, id: &str) -> MlrcResult<Version> {
    http.get_json(&format!("{BASE}/version/{id}")).await
}

#[derive(Deserialize)]
struct Project {
    #[serde(default)]
    slug: String,
}

/// Slug for a project id, for labelling dependencies the user did not name.
pub async fn project_slug(http: &HttpClient, id: &str) -> MlrcResult<String> {
    let project: Project = http
        .get_json_query(&format!("{BASE}/project/{id}"), &[])
        .await?;
    Ok(project.slug)
}

pub fn user_agent() -> &'static str {
    USER_AGENT
}
