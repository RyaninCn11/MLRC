//! Installing one version: metadata, then bytes, then state.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

/// How the caller wants Java handled. `allow_host` mirrors `java.custom`.
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
    /// `general.proxy`: passed to the installer's JVM, which does its own downloads.
    pub proxy: String,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            threads: 8,
            java: JavaPolicy::default(),
            proxy: "auto".to_string(),
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
    loader: crate::loader::fabric::Family,
    loader_version: Option<&str>,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<InstalledState> {
    root.ensure()?;

    let manifest = VersionManifest::load(client, root).await?;
    let profile = crate::loader::fabric::resolve_family(client, loader, mc, loader_version).await?;
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
            root.libraries_dir().join(&library.path),
        );
    }
    // Launcher-extracted natives all go to `java/`: that is the directory
    // `-Djava.library.path` points at, and pre-1.19 LWJGL loads its DLLs through it
    // instead of unpacking them itself. The other three areas belong to libraries
    // that ride the classpath and extract at runtime.
    for native in &plan.natives {
        let dir = crate::launch::natives::natives_root(root, id).join("java");
        add(&mut files, native, dir.join(native.file_name()));
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
    state::write_objects(&root.state_dir(id), &referenced)?;
    state.write(&InstalledState::path(root, id))?;

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
    loader: crate::loader::fabric::Family,
    loader_version: Option<&str>,
) -> MlrcResult<Preview> {
    root.ensure()?;
    let manifest = VersionManifest::load(client, root).await?;
    let profile = crate::loader::fabric::resolve_family(client, loader, mc, loader_version).await?;
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

/// Installs Forge or NeoForge by running the loader's own installer.
///
/// Those installers patch the vanilla client with a chain of processors; reimplementing
/// that is a project of its own. Instead the installer gets a launcher-shaped staging
/// directory (`launcher_profiles.json`, `versions/<mc>/<mc>.{json,jar}`), and whatever
/// it produces is adopted into the version directory.
pub async fn install_installer(
    client: &HttpClient,
    root: &DataRoot,
    mc: &str,
    installer: &crate::loader::forge::Installer,
    options: &InstallOptions,
    sink: &dyn EventSink,
) -> MlrcResult<InstalledState> {
    root.ensure()?;

    // The installer needs the vanilla client to patch.
    let parent_complete = InstalledState::load(root, mc)?
        .map(|state| state.complete)
        .unwrap_or(false);
    if !parent_complete {
        install(client, root, mc, options, sink).await?;
    }

    let parent = VersionJson::read_cached(root, mc)?.ok_or_else(|| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("metadata for {mc} is missing; reinstall it"),
        )
    })?;
    let required = parent
        .java_version
        .as_ref()
        .and_then(|java| java.major_version)
        .unwrap_or(17);

    let java = java_for_installer(root, &options.java, required)?;

    let stage = root.tmp_dir().join("installer").join(&installer.id);
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    let parent_stage = stage.join("versions").join(mc);
    std::fs::create_dir_all(&parent_stage)?;
    std::fs::write(stage.join("launcher_profiles.json"), b"{\"profiles\":{}}")?;
    std::fs::copy(
        VersionJson::cache_path(root, mc),
        parent_stage.join(format!("{mc}.json")),
    )?;
    std::fs::copy(
        root.version_dir(mc).join(format!("{mc}.jar")),
        parent_stage.join(format!("{mc}.jar")),
    )?;

    sink.emit(Event::Phase {
        phase: "installer".to_string(),
        total: None,
    });

    let bytes = crate::loader::forge::installer_bytes(client, &installer.url).await?;
    let installer_jar = stage.join("installer.jar");
    std::fs::write(&installer_jar, &bytes)?;

    // NeoForge's installer ignores `--mirror` for library downloads; seeding them
    // first is what keeps the install possible on a blocked maven host.
    crate::loader::forge::preseed_libraries(client, &stage, &installer_jar, sink).await?;

    let mut command = std::process::Command::new(&java);
    command
        // The installer creates temp directories; keep them inside the data tree.
        .arg(format!("-Djava.io.tmpdir={}", root.tmp_dir().display()))
        .args(crate::fetch::proxy::java_args_for(&options.proxy))
        .arg("-jar")
        .arg(&installer_jar)
        .arg("--installClient")
        .arg(&stage)
        .current_dir(&stage)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // The installers fetch their own libraries from their maven. Point them at the
    // mirror when one is in use, otherwise a blocked host fails the whole install.
    if client.mirror() == crate::fetch::Mirror::Bmclapi {
        command
            .arg("--mirror")
            .arg("https://bmclapi2.bangbang93.com/maven");
    }

    let log_path = root
        .logs_dir()
        .join(format!("installer-{}.log", installer.id));
    let log = std::sync::Arc::new(std::sync::Mutex::new(
        std::fs::File::create(&log_path).map_err(|err| {
            MlrcError::new(
                ErrorCode::Io,
                format!("cannot create {}: {err}", log_path.display()),
            )
        })?,
    ));

    let mut child = command.spawn().map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!(
                "cannot run the {} installer: {err}",
                installer.flavour.as_str()
            ),
        )
    })?;

    // Drain the pipes while the installer runs: they would otherwise fill up and
    // block it, and a silent four-minute install looks like a hang.
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(pump(stdout, std::sync::Arc::clone(&log)));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(pump(stderr, std::sync::Arc::clone(&log)));
    }

    sink.emit(Event::Note {
        message: format!(
            "running the {} installer; log: {}",
            installer.flavour.as_str(),
            log_path.display()
        ),
    });

    let started = std::time::Instant::now();
    let mut heartbeat = 0u64;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                return Err(MlrcError::new(
                    ErrorCode::Io,
                    format!("cannot wait for the installer: {err}"),
                ))
            }
        }

        let elapsed = started.elapsed().as_secs();
        if elapsed >= 15 && elapsed / 15 > heartbeat {
            heartbeat = elapsed / 15;
            sink.emit(Event::Note {
                message: format!("installer still running ({elapsed}s)"),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    };

    for reader in readers {
        let _ = reader.join();
    }

    // The installer's own output is the only explanation when it fails.
    let combined = std::fs::read_to_string(&log_path).unwrap_or_default();

    if !status.success() {
        let lines: Vec<&str> = combined
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let tail = lines[lines.len().saturating_sub(8)..].join(" | ");
        return Err(MlrcError::new(
            ErrorCode::LoaderUnavailable,
            format!(
                "the {} installer failed for {mc} (exit {})",
                installer.flavour.as_str(),
                status.code().unwrap_or(-1)
            ),
        )
        .with("output", tail)
        .with("log", log_path.display().to_string()));
    }

    let produced = stage
        .join("versions")
        .join(&installer.id)
        .join(format!("{}.json", installer.id));
    let produced_bytes = std::fs::read(&produced).map_err(|err| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("the installer produced no version document: {err}"),
        )
    })?;
    VersionJson::store_cached(root, &installer.id, &produced_bytes)?;

    let version_dir = root.version_dir(&installer.id);
    // Libraries are shared across versions now, so the installer's output merges into
    // the one Maven tree instead of a private copy per version.
    copy_tree(&stage.join("libraries"), &root.libraries_dir())?;
    link_tree(
        &root.version_dir(mc).join("libraries"),
        &root.libraries_dir(),
    )?;

    let staged_client = stage.join("versions").join(mc).join(format!("{mc}.jar"));
    if staged_client.is_file() {
        std::fs::copy(
            &staged_client,
            version_dir.join(format!("{}.jar", installer.id)),
        )?;
    }

    let (files, size) = dir_stats(&version_dir)?;
    let state = InstalledState {
        id: installer.id.clone(),
        complete: true,
        java_component: parent
            .java_version
            .as_ref()
            .map(|java| java.component.clone()),
        java_major: parent
            .java_version
            .as_ref()
            .and_then(|java| java.major_version),
        files: files as u32,
        bytes: size,
        installed_at: now(),
    };
    state.write(&InstalledState::path(root, &installer.id))?;
    // Nothing here came through the content-addressed store, but the empty list
    // keeps `cache prune` from refusing to run.
    state::write_objects(&root.state_dir(&installer.id), &BTreeSet::new())?;

    sink.emit(Event::Done {
        message: format!("installed {}", installer.id),
        ok: true,
    });

    Ok(state)
}

/// The installer only needs a working JVM; the version's own runtime is preferred but
/// any managed one will do, since processors run on the JVM rather than the game.
fn java_for_installer(root: &DataRoot, policy: &JavaPolicy, required: u32) -> MlrcResult<PathBuf> {
    if let Some(installed) = discover::managed(root, required) {
        return Ok(installed.executable);
    }

    let mut newest: Option<(u32, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(root.java_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(major) = name
                .strip_prefix("jdk-")
                .and_then(|rest| rest.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(installed) = discover::managed(root, major) {
                if newest
                    .as_ref()
                    .map(|(best, _)| major > *best)
                    .unwrap_or(true)
                {
                    newest = Some((major, installed.executable));
                }
            }
        }
    }
    if let Some((_, executable)) = newest {
        return Ok(executable);
    }
    let host = if policy.allow_host {
        discover::host(&policy.host_path)
    } else {
        None
    };
    if let Some(host) = host {
        return Ok(host.executable);
    }

    Err(MlrcError::new(
        ErrorCode::JavaMissing,
        format!(
            "java {required} is needed to run the installer; run `mlrc java install {required}`"
        ),
    )
    .with("major", required as u64))
}

/// Copies a child process stream into the installer log as it arrives.
fn pump(
    stream: impl Read + Send + 'static,
    log: Arc<Mutex<std::fs::File>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if let Ok(mut file) = log.lock() {
                let _ = writeln!(file, "{line}");
            }
        }
    })
}

fn copy_tree(from: &Path, to: &Path) -> MlrcResult<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;

    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn link_tree(from: &Path, to: &Path) -> MlrcResult<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;

    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else if !target.exists() && std::fs::hard_link(entry.path(), &target).is_err() {
            // Different volume, or a filesystem without hardlinks.
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn dir_stats(dir: &Path) -> MlrcResult<(usize, u64)> {
    let mut files = 0usize;
    let mut bytes = 0u64;

    if !dir.is_dir() {
        return Ok((files, bytes));
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let (nested_files, nested_bytes) = dir_stats(&entry.path())?;
            files += nested_files;
            bytes += nested_bytes;
        } else {
            files += 1;
            bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }

    Ok((files, bytes))
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_default()
}
