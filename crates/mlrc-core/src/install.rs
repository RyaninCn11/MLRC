//! Installing one version: metadata, then bytes, then state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::events::{Event, EventSink};
use crate::fetch::download::{DownloadTask, Downloader};
use crate::fetch::HttpClient;
use crate::java::{discover, runtime};
use crate::meta::asset::{self, AssetIndexJson};
use crate::meta::inherit;
use crate::meta::manifest::VersionManifest;
use crate::meta::rule::Env;
use crate::meta::version::{Artifact, AssetIndex, VersionJson};
use crate::plan::install as plan;
use crate::port::state::{self, InstalledState};
use crate::port::DataRoot;
use crate::store::ObjectStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JavaMode {
    /// Report the requirement and let the caller decide (prompt, dialog, exit 5).
    #[default]
    Auto,
    /// Provision the runtime as part of the install.
    Install,
    /// Continue without Java; launching will fail later.
    Skip,
}

/// How the caller wants Java handled. `allow_host` mirrors `customJava`.
#[derive(Debug, Clone, Default)]
pub struct JavaPolicy {
    pub mode: JavaMode,
    pub allow_host: bool,
    pub host_path: String,
}

impl JavaPolicy {
    pub fn satisfies(&self, root: &DataRoot, required: u32) -> bool {
        if discover::managed(root, required).is_some() {
            return true;
        }
        if !self.allow_host {
            return false;
        }
        discover::host(&self.host_path)
            .map(|installation| installation.major == required)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub threads: usize,
    pub java: JavaPolicy,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            threads: 8,
            java: JavaPolicy::default(),
        }
    }
}

pub async fn install(
    client: &HttpClient,
    root: &DataRoot,
    id: &str,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<InstalledState> {
    root.ensure()?;

    let manifest = VersionManifest::load(client, root).await?;
    let entry = manifest
        .find(id)
        .ok_or_else(|| {
            MlrcError::new(ErrorCode::VersionUnknown, format!("unknown version: {id}"))
                .with("id", id)
        })?
        .clone();

    let version = VersionJson::load(client, root, id, &entry.url).await?;

    install_prepared(client, root, &manifest, id, version, options, sink).await
}

/// Installs a loader version: a document that inherits from a vanilla one.
///
/// The profile is cached under its own id (`fabric-loader-<loader>-<mc>`), so launch
/// finds it exactly where a vanilla document would be.
pub async fn install_loader(
    client: &HttpClient,
    root: &DataRoot,
    mc: &str,
    loader_version: Option<&str>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<InstalledState> {
    root.ensure()?;

    let manifest = VersionManifest::load(client, root).await?;
    let profile = crate::loader::fabric::resolve(client, mc, loader_version).await?;
    VersionJson::store_cached(root, &profile.id, &profile.document)?;

    install_prepared(
        client,
        root,
        &manifest,
        &profile.id,
        profile.version,
        options,
        sink,
    )
    .await
}

async fn install_prepared(
    client: &HttpClient,
    root: &DataRoot,
    manifest: &VersionManifest,
    id: &str,
    version: VersionJson,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<InstalledState> {
    let document = resolve_inheritance(client, root, manifest, version).await?;

    let env = Env::detect();
    let plan = plan::build(&document, &env)?;

    if let Some(java) = plan.java.clone() {
        if !options.java.satisfies(root, java.major) {
            match options.java.mode {
                JavaMode::Skip => sink.emit(Event::Warning {
                    code: "E_JAVA_MISSING".to_string(),
                    message: format!(
                        "java {} ({}) is not installed; launching will fail until it is",
                        java.major, java.component
                    ),
                }),
                JavaMode::Auto => {
                    sink.emit(Event::JavaRequired {
                        component: java.component.clone(),
                        major: java.major,
                        download_bytes: 0,
                    });
                    return Err(java_required(&java));
                }
                JavaMode::Install => {
                    let index = runtime::load_index(client, root).await?;
                    let platform = runtime::platform_key();
                    let resolved = runtime::resolve(
                        &index,
                        platform,
                        runtime::Target::Component(&java.component),
                    )
                    .or_else(|| {
                        runtime::resolve(&index, platform, runtime::Target::Major(java.major))
                    })
                    .ok_or_else(|| {
                        MlrcError::new(
                            ErrorCode::JavaMissing,
                            format!("no java runtime for {} on {platform}", java.component),
                        )
                    })?;

                    runtime::install(client, root, &resolved, options.threads, sink).await?;
                }
            }
        }
    }

    let version_dir = root.version_dir(id);
    let mut files: BTreeMap<String, DownloadTask> = BTreeMap::new();

    if let Some(client_jar) = &plan.client {
        add(
            &mut files,
            client_jar,
            version_dir.join(format!("{id}.jar")),
        );
    }
    for library in &plan.libraries {
        add(
            &mut files,
            library,
            version_dir.join("libraries").join(&library.path),
        );
    }
    for native in &plan.natives {
        add(
            &mut files,
            native,
            version_dir.join("natives").join(native.file_name()),
        );
    }
    if let Some(logging) = &plan.logging {
        add(
            &mut files,
            logging,
            version_dir.join("logging").join(logging.file_name()),
        );
    }
    if let Some(index) = &plan.asset_index {
        add(
            &mut files,
            &index_artifact(index),
            AssetIndexJson::index_path(root, &index.id),
        );
    }

    let store = ObjectStore::new(root);
    let downloader = Downloader {
        client,
        store: &store,
        sink,
        threads: options.threads,
        attempts: 3,
    };

    let mut referenced: std::collections::BTreeSet<String> = files.keys().cloned().collect();
    let mut summary = downloader.run(files.into_values().collect()).await?;

    // Assets are only known after the index is on disk.
    if let Some(index) = &plan.asset_index {
        let index_path = AssetIndexJson::index_path(root, &index.id);
        let bytes = std::fs::read(&index_path).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot read {}: {err}", index_path.display()),
            )
        })?;
        let index_json = AssetIndexJson::parse(&bytes, &index.id)?;

        if index_json.virtual_assets.unwrap_or(false)
            || index_json.map_to_resources.unwrap_or(false)
        {
            sink.emit(Event::Warning {
                code: "E_ASSET_LAYOUT_LEGACY".to_string(),
                message: format!(
                    "asset index {} uses a pre-1.7 layout; objects are installed but not mapped by name",
                    index.id
                ),
            });
        }

        let mut assets: BTreeMap<String, DownloadTask> = BTreeMap::new();
        for object in index_json.objects.values() {
            add_raw(
                &mut assets,
                &object.hash,
                &asset::object_url(&object.hash),
                object.size,
                asset::object_path(root, &object.hash),
                false,
            );
        }

        referenced.extend(assets.keys().cloned());
        let asset_summary = downloader.run(assets.into_values().collect()).await?;
        summary.files += asset_summary.files;
        summary.bytes += asset_summary.bytes;
        summary.reused += asset_summary.reused;
    }

    let state = InstalledState {
        id: id.to_string(),
        complete: true,
        java_component: plan.java.as_ref().map(|java| java.component.clone()),
        java_major: plan.java.as_ref().map(|java| java.major),
        files: summary.files as u32,
        bytes: summary.bytes,
        installed_at: now(),
    };
    state::write_objects(&version_dir, &referenced)?;
    state.write(&version_dir.join(InstalledState::FILE_NAME))?;

    sink.emit(Event::Done {
        message: format!(
            "installed {id}: {} files, {} reused",
            summary.files, summary.reused
        ),
        ok: true,
    });

    Ok(state)
}

/// What an install would do, without doing any of it.
pub struct Preview {
    pub id: String,
    pub kind: String,
    pub release_time: String,
    pub plan: plan::InstallPlan,
}

pub async fn preview(client: &HttpClient, root: &DataRoot, id: &str) -> MlrcResult<Preview> {
    root.ensure()?;
    let manifest = VersionManifest::load(client, root).await?;
    let entry = manifest
        .find(id)
        .ok_or_else(|| {
            MlrcError::new(ErrorCode::VersionUnknown, format!("unknown version: {id}"))
                .with("id", id)
        })?
        .clone();

    let version = VersionJson::load(client, root, id, &entry.url).await?;
    let document = resolve_inheritance(client, root, &manifest, version).await?;
    make_preview(document)
}

pub async fn preview_loader(
    client: &HttpClient,
    root: &DataRoot,
    mc: &str,
    loader_version: Option<&str>,
) -> MlrcResult<Preview> {
    root.ensure()?;
    let manifest = VersionManifest::load(client, root).await?;
    let profile = crate::loader::fabric::resolve(client, mc, loader_version).await?;
    let document = resolve_inheritance(client, root, &manifest, profile.version).await?;
    make_preview(document)
}

fn make_preview(document: VersionJson) -> MlrcResult<Preview> {
    let plan = plan::build(&document, &Env::detect())?;
    Ok(Preview {
        id: document.id.clone(),
        kind: document.kind.clone(),
        release_time: document.release_time.clone(),
        plan,
    })
}

/// Loads a document and resolves its parents, preferring cached copies.
///
/// Launch uses this: after an install, both the loader profile and its vanilla
/// parent are cached, so resolution needs no network at all.
pub async fn resolve_document(
    client: &HttpClient,
    root: &DataRoot,
    id: &str,
) -> MlrcResult<VersionJson> {
    let mut document = VersionJson::read_cached(root, id)?.ok_or_else(|| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("metadata for {id} is missing; reinstall it"),
        )
        .with("id", id)
    })?;

    let mut depth = 0;
    while let Some(parent_id) = document.inherits_from.clone() {
        depth += 1;
        if depth > 8 {
            return Err(MlrcError::new(
                ErrorCode::MetaShape,
                format!("inheritance chain too deep at {id}"),
            ));
        }

        let parent = match VersionJson::read_cached(root, &parent_id)? {
            Some(cached) => cached,
            None => {
                let manifest = VersionManifest::load(client, root).await?;
                let entry = manifest.find(&parent_id).ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::VersionUnknown,
                        format!("parent version {parent_id} is not in the manifest"),
                    )
                    .with("id", parent_id.clone())
                })?;
                VersionJson::load(client, root, &parent_id, &entry.url).await?
            }
        };

        document = inherit::merge(&document, &parent);
    }

    Ok(document)
}

async fn resolve_inheritance(
    client: &HttpClient,
    root: &DataRoot,
    manifest: &VersionManifest,
    mut document: VersionJson,
) -> MlrcResult<VersionJson> {
    let mut depth = 0;
    while let Some(parent_id) = document.inherits_from.clone() {
        depth += 1;
        if depth > 8 {
            return Err(MlrcError::new(
                ErrorCode::MetaShape,
                format!("inheritance chain too deep at {}", document.id),
            ));
        }
        let parent_entry = manifest.find(&parent_id).ok_or_else(|| {
            MlrcError::new(
                ErrorCode::VersionUnknown,
                format!("parent version {parent_id} is not in the manifest"),
            )
            .with("id", parent_id.clone())
        })?;
        let parent = VersionJson::load(client, root, &parent_id, &parent_entry.url).await?;
        document = inherit::merge(&document, &parent);
    }
    Ok(document)
}

fn java_required(java: &plan::JavaRequirement) -> MlrcError {
    MlrcError::new(
        ErrorCode::JavaMissing,
        format!("java {} ({}) is not installed", java.major, java.component),
    )
    .with("component", java.component.clone())
    .with("major", java.major as u64)
}

fn index_artifact(index: &AssetIndex) -> Artifact {
    Artifact {
        path: format!("{}.json", index.id),
        sha1: index.sha1.clone(),
        size: index.size,
        url: index.url.clone(),
    }
}

fn add(groups: &mut BTreeMap<String, DownloadTask>, artifact: &Artifact, dest: PathBuf) {
    add_raw(
        groups,
        &artifact.sha1,
        &artifact.url,
        artifact.size,
        dest,
        true,
    );
}

fn add_raw(
    groups: &mut BTreeMap<String, DownloadTask>,
    sha1: &str,
    url: &str,
    size: u64,
    dest: PathBuf,
    required: bool,
) {
    // Loader libraries may publish coordinates without a hash; those are keyed by URL
    // so they are still deduplicated within one install.
    let key = if sha1.is_empty() {
        url.to_string()
    } else {
        sha1.to_string()
    };

    if let Some(task) = groups.get_mut(&key) {
        if !task.dests.contains(&dest) {
            task.dests.push(dest);
        }
        return;
    }

    groups.insert(
        key,
        DownloadTask {
            sha1: sha1.to_string(),
            url: url.to_string(),
            size,
            dests: vec![dest],
            required,
        },
    );
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_default()
}
