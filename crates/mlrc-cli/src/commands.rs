//! Subcommand handlers. Thin by design: resolve paths, call the engine, render.

use std::collections::BTreeSet;
use std::fs;
use std::io::{IsTerminal, Write};
use std::time::{Duration, SystemTime};

use serde_json::json;

use mlrc_core::auth;
use mlrc_core::doctor::{self as doc, Status};
use mlrc_core::error::{ErrorCode, MlrcError, MlrcResult};
use mlrc_core::fetch::mirror::{self, Mirror};
use mlrc_core::fetch::HttpClient;
use mlrc_core::install::{
    install as run_install, InstallOptions, JavaMode as CoreJavaMode, JavaPolicy,
};
use mlrc_core::java::{discover, runtime};
use mlrc_core::launch::{self, LaunchOptions};
use mlrc_core::meta::manifest::{VersionEntry, VersionManifest};
use mlrc_core::meta::rule::Env;
use mlrc_core::meta::version::{Artifact, VersionJson};
use mlrc_core::plan::install::{self, InstallPlan};
use mlrc_core::port::config::{self, Config, KEYS as CONFIG_KEYS};
use mlrc_core::port::state::{self, InstalledState};
use mlrc_core::port::DataRoot;

use crate::cli::{
    CacheCommand, Cli, Command, ConfigCommand, JavaCommand, JavaMode as CliJavaMode, MirrorArg,
};
use crate::progress::ProgressSink;
use crate::render::Out;

pub async fn run(cli: &Cli, out: &Out) -> MlrcResult<()> {
    match &cli.command {
        Command::Doctor => cmd_doctor(cli, out).await,
        Command::Config { command } => cmd_config(cli, out, command),
        Command::Use { version } => cmd_use(cli, out, version),
        Command::List { all } => cmd_list(cli, out, *all).await,
        Command::Install {
            version,
            dry_run,
            threads,
            java,
            yes,
        } => cmd_install(cli, out, version, *dry_run, *threads, *java, *yes).await,
        Command::Launch {
            version,
            offline,
            username,
            width,
            height,
            dry_run,
            game_args,
        } => {
            cmd_launch(
                cli,
                out,
                version.as_deref(),
                *offline,
                username.as_deref(),
                *width,
                *height,
                *dry_run,
                game_args,
            )
            .await
        }
        Command::Uninstall { version } => cmd_uninstall(cli, out, version),
        Command::Cache { command } => cmd_cache(cli, out, command),
        Command::Java { command } => cmd_java(cli, out, command).await,
        Command::Auth { .. } => Err(not_implemented("auth", "M5")),
    }
}

async fn cmd_doctor(cli: &Cli, out: &Out) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;
    let (client, _) = open_client(cli, &root).await?;
    let checks = doc::run(&root, &cfg, &client).await;

    for check in &checks {
        out.human(format!(
            "{:<14}{}",
            format!("{}:", check.name),
            check.detail
        ));
        out.json_event(json!({
            "schema": 1,
            "event": "check",
            "name": check.name,
            "status": check.status.as_str(),
            "detail": check.detail,
        }));
    }

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    if failed > 0 {
        return Err(MlrcError::new(
            ErrorCode::Doctor,
            format!("{failed} check(s) failed"),
        ));
    }

    out.json_event(json!({"schema": 1, "event": "done", "ok": true}));
    Ok(())
}

fn cmd_config(cli: &Cli, out: &Out, command: &ConfigCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    let path = root.config_path();

    match command {
        ConfigCommand::List => {
            let cfg = Config::load(&path)?;
            for key in CONFIG_KEYS {
                let value = cfg.get(key)?;
                out.human(format!("{key} = {value}"));
                out.json_event(json!({"schema": 1, "event": "config", "key": key, "value": value}));
            }
            Ok(())
        }
        ConfigCommand::Get { key } => {
            let cfg = Config::load(&path)?;
            let value = cfg.get(key)?;
            out.human(value.clone());
            out.json_event(json!({"schema": 1, "event": "config", "key": key, "value": value}));
            Ok(())
        }
        ConfigCommand::Set { key, value } => {
            let mut cfg = Config::load(&path)?;
            cfg.set(key, value)?;
            cfg.save(&path)?;
            let stored = cfg.get(key)?;
            out.human(format!("{key} = {stored}"));
            out.json_event(json!({"schema": 1, "event": "config", "key": key, "value": stored}));
            Ok(())
        }
    }
}

fn cmd_use(cli: &Cli, out: &Out, version: &str) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let state_path = root.version_dir(version).join(InstalledState::FILE_NAME);

    match InstalledState::read(&state_path)? {
        Some(state) if state.complete => {
            let mut cfg = Config::load(&root.config_path())?;
            cfg.general.default_version = version.to_string();
            cfg.save(&root.config_path())?;
            out.human(format!("default version: {version}"));
            out.json_event(json!({"schema": 1, "event": "done", "default_version": version}));
            Ok(())
        }
        Some(_) => Err(MlrcError::new(
            ErrorCode::VersionIncomplete,
            format!("version {version} is incomplete; run `mlrc install {version}`"),
        )),
        None => Err(MlrcError::new(
            ErrorCode::VersionUnknown,
            format!("version {version} is not installed; run `mlrc install {version}`"),
        )),
    }
}

async fn cmd_list(cli: &Cli, out: &Out, all: bool) -> MlrcResult<()> {
    if all {
        let root = DataRoot::resolve(cli.data_dir.as_deref())?;
        root.ensure()?;
        let (client, source) = open_client(cli, &root).await?;
        announce_source(out, source);
        let manifest = VersionManifest::load(&client, &root).await?;

        out.human(format!(
            "{} versions known; latest release {}, latest snapshot {}",
            manifest.versions.len(),
            manifest.latest.release,
            manifest.latest.snapshot
        ));
        for version in &manifest.versions {
            out.human(format!(
                "{:<26}{:<10}{}",
                version.id, version.kind, version.release_time
            ));
            out.json_event(json!({
                "schema": 1,
                "event": "version",
                "id": version.id,
                "type": version.kind,
                "release_time": version.release_time,
            }));
        }
        return Ok(());
    }

    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;

    let mut rows: Vec<(String, Option<InstalledState>)> = Vec::new();
    let dir = root.versions_dir();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let state = InstalledState::read(&entry.path().join(InstalledState::FILE_NAME))?;
            rows.push((id, state));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if out.is_json() {
        let items: Vec<_> = rows
            .iter()
            .map(|(id, state)| {
                json!({
                    "id": id,
                    "complete": state.as_ref().map(|s| s.complete).unwrap_or(false),
                    "java_major": state.as_ref().and_then(|s| s.java_major),
                    "bytes": state.as_ref().map(|s| s.bytes).unwrap_or(0),
                    "default": cfg.general.default_version == *id,
                })
            })
            .collect();
        out.json_event(json!({"schema": 1, "event": "versions", "versions": items}));
        return Ok(());
    }

    if rows.is_empty() {
        out.human("no versions installed");
        return Ok(());
    }

    out.human(format!(
        " {:<23}{:>6}{:>12}  {}",
        "ID", "JAVA", "SIZE", "STATUS"
    ));
    for (id, state) in &rows {
        let java = state
            .as_ref()
            .and_then(|s| s.java_major)
            .map(|major| major.to_string())
            .unwrap_or_else(|| "-".to_string());
        let size = human_bytes(state.as_ref().map(|s| s.bytes).unwrap_or(0));
        let status = match state {
            Some(s) if s.complete => "complete",
            Some(_) => "incomplete",
            None => "unknown",
        };
        let marker = if cfg.general.default_version == *id {
            "*"
        } else {
            " "
        };
        out.human(format!(
            "{marker}{:<23}{:>6}{:>12}  {}",
            id, java, size, status
        ));
    }

    Ok(())
}

async fn cmd_install(
    cli: &Cli,
    out: &Out,
    version: &str,
    dry_run: bool,
    threads: usize,
    java_mode: CliJavaMode,
    yes: bool,
) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let (client, source) = open_client(cli, &root).await?;
    announce_source(out, source);

    out.human(format!("resolving {version} ..."));
    let manifest = VersionManifest::load(&client, &root).await?;
    let entry = manifest
        .find(version)
        .ok_or_else(|| {
            MlrcError::new(
                ErrorCode::VersionUnknown,
                format!("unknown version: {version}"),
            )
            .with("id", version)
        })?
        .clone();

    let document = VersionJson::load(&client, &root, version, &entry.url).await?;
    let plan = install::build(&document, &Env::detect())?;

    if dry_run {
        print_plan(out, &entry, &plan, &root);
        return Ok(());
    }

    let cfg = Config::load(&root.config_path())?;
    let mut mode = match java_mode {
        CliJavaMode::Auto => CoreJavaMode::Auto,
        CliJavaMode::Install => CoreJavaMode::Install,
        CliJavaMode::Skip => CoreJavaMode::Skip,
    };
    if yes {
        mode = CoreJavaMode::Install;
    }
    let policy = JavaPolicy {
        mode,
        allow_host: cfg.java.custom,
        host_path: cfg.java.path.clone(),
    };

    let live = !cli.json && !cli.quiet;
    let sink = ProgressSink::new(live);

    let state = match run_install(
        &client,
        &root,
        version,
        &InstallOptions {
            threads,
            java: policy.clone(),
        },
        &sink,
    )
    .await
    {
        Ok(state) => state,
        Err(err) if err.code == ErrorCode::JavaMissing => {
            let component = err
                .context
                .get("component")
                .and_then(|value| value.as_str())
                .unwrap_or("java")
                .to_string();
            let major = err
                .context
                .get("major")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);

            // Machine callers get a decision request, never a prompt.
            if cli.json || !std::io::stdin().is_terminal() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "java_required",
                    "component": component,
                    "major": major,
                }));
                return Err(err);
            }

            eprintln!("java {major} ({component}) is not installed.");
            eprint!("download it now? [Y/n] ");
            let _ = std::io::stderr().flush();

            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            let accepted = matches!(
                answer.trim().to_ascii_lowercase().as_str(),
                "" | "y" | "yes"
            );
            if !accepted {
                return Err(err);
            }

            run_install(
                &client,
                &root,
                version,
                &InstallOptions {
                    threads,
                    java: JavaPolicy {
                        mode: CoreJavaMode::Install,
                        ..policy
                    },
                },
                &sink,
            )
            .await?
        }
        Err(err) => return Err(err),
    };
    sink.finish();

    out.human(format!(
        "installed {version}: {} files, {}",
        state.files,
        human_bytes(state.bytes)
    ));
    out.json_event(json!({
        "schema": 1,
        "event": "installed",
        "id": version,
        "files": state.files,
        "bytes": state.bytes,
        "java_major": state.java_major,
    }));

    Ok(())
}

fn print_plan(out: &Out, entry: &VersionEntry, plan: &InstallPlan, root: &DataRoot) {
    let java_installed = plan
        .java
        .as_ref()
        .map(|java| root.jdk_dir(java.major).is_dir())
        .unwrap_or(false);

    out.human(format!(
        "version      : {} ({}, {})",
        plan.id, entry.kind, entry.release_time
    ));
    out.human(format!("main class   : {}", plan.main_class));

    if let Some(java) = &plan.java {
        out.human(format!(
            "java         : {} ({}), {}",
            java.major,
            java.component,
            if java_installed {
                "installed"
            } else {
                "not installed"
            }
        ));
    }

    let client = plan
        .client
        .as_ref()
        .map(|artifact| human_bytes(artifact.size))
        .unwrap_or_else(|| "-".to_string());
    out.human(format!("client jar   : {client}"));
    out.human(format!(
        "libraries    : {} files, {}",
        plan.libraries.len(),
        human_bytes(total(&plan.libraries))
    ));
    out.human(format!(
        "natives      : {} files, {}",
        plan.natives.len(),
        human_bytes(total(&plan.natives))
    ));

    if let Some(index) = &plan.asset_index {
        out.human(format!(
            "asset index  : {} ({}); asset objects are not counted",
            index.id,
            human_bytes(index.size)
        ));
    }

    out.human(format!(
        "total        : {} files, {} (excluding asset objects)",
        plan.files,
        human_bytes(plan.bytes)
    ));

    out.json_event(json!({
        "schema": 1,
        "event": "plan",
        "id": plan.id,
        "java": plan.java.as_ref().map(|java| json!({
            "component": java.component,
            "major": java.major,
            "installed": java_installed,
        })),
        "files": plan.files,
        "bytes": plan.bytes,
        "libraries": plan.libraries.len(),
        "natives": plan.natives.len(),
    }));
}

fn total(artifacts: &[Artifact]) -> u64 {
    artifacts.iter().map(|artifact| artifact.size).sum()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Builds a client for the configured (or requested) source. `auto` costs one
/// probe and then persists its decision, so the next run is instant.
async fn open_client(cli: &Cli, root: &DataRoot) -> MlrcResult<(HttpClient, Mirror)> {
    let mut cfg = Config::load(&root.config_path())?;

    let preference = match cli.mirror {
        Some(MirrorArg::Auto) => config::Mirror::Auto,
        Some(MirrorArg::Official) => config::Mirror::Official,
        Some(MirrorArg::Bmclapi) => config::Mirror::Bmclapi,
        None => cfg.general.mirror,
    };

    let resolved = match Mirror::from_config(preference) {
        Some(mirror) => mirror,
        None => {
            let chosen = mirror::probe().await;
            if cfg.general.mirror != chosen.to_config() {
                cfg.general.mirror = chosen.to_config();
                cfg.save(&root.config_path())?;
            }
            chosen
        }
    };

    Ok((HttpClient::new(resolved)?, resolved))
}

/// BMCLAPI's terms require the download UI to name the source.
fn announce_source(out: &Out, mirror: Mirror) {
    if mirror == Mirror::Bmclapi {
        out.human("download source: BMCLAPI (https://bmclapi2.bangbang93.com)");
        out.json_event(json!({
            "schema": 1,
            "event": "source",
            "mirror": "bmclapi",
            "url": "https://bmclapi2.bangbang93.com",
        }));
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_launch(
    cli: &Cli,
    out: &Out,
    version: Option<&str>,
    _offline: bool,
    username: Option<&str>,
    width: Option<u32>,
    height: Option<u32>,
    dry_run: bool,
    game_args: &[String],
) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;

    let id = match version {
        Some(id) => id.to_string(),
        None if !cfg.general.default_version.is_empty() => cfg.general.default_version.clone(),
        None => {
            return Err(MlrcError::new(
                ErrorCode::Usage,
                "no version given and no default set; run `mlrc use <version>`",
            ))
        }
    };

    match InstalledState::read(&root.version_dir(&id).join(InstalledState::FILE_NAME))? {
        Some(state) if state.complete => {}
        Some(_) => {
            return Err(MlrcError::new(
                ErrorCode::VersionIncomplete,
                format!("version {id} is incomplete; run `mlrc install {id}`"),
            ))
        }
        None => {
            return Err(MlrcError::new(
                ErrorCode::VersionUnknown,
                format!("version {id} is not installed; run `mlrc install {id}`"),
            ))
        }
    }

    let document = VersionJson::read_cached(&root, &id)?.ok_or_else(|| {
        MlrcError::new(
            ErrorCode::MetaShape,
            format!("metadata for {id} is missing; reinstall it"),
        )
    })?;

    let required = document
        .java_version
        .as_ref()
        .and_then(|java| java.major_version)
        .ok_or_else(|| {
            MlrcError::new(
                ErrorCode::JavaMetaMissing,
                format!("version {id} does not declare a java version"),
            )
        })?;

    let java = discover::managed(&root, required)
        .or_else(|| {
            if cfg.java.custom {
                discover::host(&cfg.java.path)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            MlrcError::new(
                ErrorCode::JavaMissing,
                format!("java {required} is not installed; run `mlrc java install {required}`"),
            )
            .with("major", required as u64)
        })?;

    let plan = install::build(&document, &Env::detect())?;
    launch::natives::extract(&plan, &root.version_dir(&id))?;

    let identity = auth::offline(username.unwrap_or("Player"));
    let options = LaunchOptions {
        width,
        height,
        extra_game_args: game_args.to_vec(),
        jvm_args: Vec::new(),
    };
    let plan = launch::build(&document, &plan, &root, &identity, &java, &options)?;

    if dry_run {
        print_launch(out, &plan);
        return Ok(());
    }

    out.human(format!("launching {id} as {}", identity.name));
    let code = launch::spawn::spawn(&plan)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn print_launch(out: &Out, plan: &launch::LaunchPlan) {
    let argv = masked_argv(plan);

    out.human(format!("cwd  : {}", plan.cwd.display()));
    out.human(format!("main : {}", plan.main_class));
    out.human("argv :");
    for argument in &argv {
        out.human(format!("  {argument}"));
    }

    out.json_event(json!({
        "schema": 1,
        "event": "launch",
        "cwd": plan.cwd.display().to_string(),
        "main_class": plan.main_class,
        "argv": argv,
    }));
}

/// `--accessToken` is followed by the token; nothing else in the argv is secret.
fn masked_argv(plan: &launch::LaunchPlan) -> Vec<String> {
    let mut masked = Vec::new();
    let mut previous = String::new();
    for argument in plan.argv() {
        if previous == "--accessToken" {
            masked.push("***".to_string());
        } else {
            masked.push(argument.clone());
        }
        previous = argument;
    }
    masked
}

fn cmd_uninstall(cli: &Cli, out: &Out, version: &str) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;

    let dir = root.version_dir(version);
    let state = InstalledState::read(&dir.join(InstalledState::FILE_NAME))?;
    if state.is_none() && !dir.exists() {
        return Err(MlrcError::new(
            ErrorCode::VersionUnknown,
            format!("version {version} is not installed"),
        ));
    }

    fs::remove_dir_all(&dir)?;

    let mut cfg = Config::load(&root.config_path())?;
    if cfg.general.default_version == version {
        cfg.general.default_version.clear();
        cfg.save(&root.config_path())?;
        out.human("default version cleared");
    }

    out.human(format!("removed version {version}"));
    out.json_event(json!({"schema": 1, "event": "done", "removed_version": version}));
    Ok(())
}

fn cmd_cache(cli: &Cli, out: &Out, command: &CacheCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;

    match command {
        CacheCommand::Prune { keep_days } => prune_cache(out, &root, *keep_days),
    }
}

/// Drops unreferenced objects from the content-addressed store.
///
/// "Referenced" comes from the `objects.txt` each install writes, not from link
/// counts: every version and every managed JDK lists the hashes it pulled.
fn prune_cache(out: &Out, root: &DataRoot, keep_days: Option<u32>) -> MlrcResult<()> {
    let cache = root.cache_dir();
    if !cache.is_dir() {
        out.human("cache is empty");
        return Ok(());
    }

    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut stale: Vec<String> = Vec::new();
    for id in subdirs(&root.versions_dir()) {
        let dir = root.version_dir(&id);
        let objects = state::read_objects(&dir)?;
        if objects.is_empty() && dir.join(InstalledState::FILE_NAME).is_file() {
            // Installed before this file existed: pruning would treat the whole
            // cache as garbage.
            stale.push(id);
        }
        referenced.extend(objects);
    }
    for jdk in subdirs(&root.java_dir()) {
        referenced.extend(state::read_objects(&root.java_dir().join(&jdk))?);
    }

    if !stale.is_empty() {
        return Err(MlrcError::new(
            ErrorCode::CacheStale,
            format!(
                "cannot prune: {} has no object list; run `mlrc install <id>` once to record it",
                stale.join(", ")
            ),
        ));
    }

    let cutoff =
        keep_days.map(|days| SystemTime::now() - Duration::from_secs(u64::from(days) * 86_400));

    let mut removed = 0usize;
    let mut freed = 0u64;
    let mut kept = 0usize;

    for bucket in fs::read_dir(&cache)? {
        let bucket = bucket?;
        if !bucket.file_type()?.is_dir() {
            continue;
        }

        let mut still_used = false;
        for entry in fs::read_dir(bucket.path())? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                still_used = true;
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata()?;
            let recent = cutoff
                .map(|cutoff| {
                    metadata
                        .modified()
                        .map(|time| time >= cutoff)
                        .unwrap_or(false)
                })
                .unwrap_or(false);

            if referenced.contains(&name) || recent {
                kept += 1;
                still_used = true;
                continue;
            }

            freed += metadata.len();
            fs::remove_file(entry.path())?;
            removed += 1;
        }

        if !still_used {
            let _ = fs::remove_dir(bucket.path());
        }
    }

    out.human(format!(
        "pruned {removed} object(s), {} freed, {kept} kept",
        human_bytes(freed)
    ));
    out.json_event(json!({
        "schema": 1,
        "event": "done",
        "pruned": removed,
        "freed_bytes": freed,
        "kept": kept,
        "referenced": referenced.len(),
    }));
    Ok(())
}

fn subdirs(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect()
}

async fn cmd_java(cli: &Cli, out: &Out, command: &JavaCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;

    match command {
        JavaCommand::List => list_java(out, &root),
        JavaCommand::Install { target, .. } => install_java(cli, out, &root, target).await,
        JavaCommand::Remove { major } => remove_java(out, &root, *major),
    }
}

fn list_java(out: &Out, root: &DataRoot) -> MlrcResult<()> {
    let mut rows: Vec<(u32, String, String, u64)> = Vec::new();

    let dir = root.java_dir();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(major) = name
                .strip_prefix("jdk-")
                .and_then(|rest| rest.parse::<u32>().ok())
            else {
                continue;
            };
            let home = entry.path();
            let metadata = read_java_metadata(&home);
            rows.push((major, metadata.0, metadata.1, dir_size(&home)));
        }
    }
    rows.sort_by_key(|row| row.0);

    if out.is_json() {
        let items: Vec<_> = rows
            .iter()
            .map(|(major, component, version, bytes)| {
                json!({
                    "major": major,
                    "component": component,
                    "version": version,
                    "bytes": bytes,
                })
            })
            .collect();
        out.json_event(json!({"schema": 1, "event": "java", "runtimes": items}));
        return Ok(());
    }

    if rows.is_empty() {
        out.human("no managed java runtimes");
    } else {
        out.human(format!(
            " {:<6}{:<26}{:<22}{}",
            "MAJOR", "COMPONENT", "VERSION", "SIZE"
        ));
        for (major, component, version, bytes) in &rows {
            out.human(format!(
                " {:<6}{:<26}{:<22}{}",
                major,
                component,
                version,
                human_bytes(*bytes)
            ));
        }
    }

    let cfg = Config::load(&root.config_path())?;
    if cfg.java.custom {
        match discover::host(&cfg.java.path) {
            Some(host) => out.human(format!(
                "host java: {} at {} (customJava=true)",
                host.major,
                host.executable.display()
            )),
            None => out.human("host java: customJava=true but no usable JDK was found"),
        }
    } else {
        out.human("host java: ignored (customJava=false)");
    }

    Ok(())
}

async fn install_java(cli: &Cli, out: &Out, root: &DataRoot, target: &str) -> MlrcResult<()> {
    let (client, source) = open_client(cli, root).await?;
    announce_source(out, source);

    let index = runtime::load_index(&client, root).await?;
    let platform = runtime::platform_key();
    let resolved = match target.parse::<u32>() {
        Ok(major) => runtime::resolve(&index, platform, runtime::Target::Major(major)),
        Err(_) => runtime::resolve(&index, platform, runtime::Target::Component(target)),
    }
    .ok_or_else(|| {
        MlrcError::new(
            ErrorCode::JavaMissing,
            format!("no java runtime matches {target} on {platform}"),
        )
    })?;

    out.human(format!(
        "installing java {} ({}) ...",
        resolved.major, resolved.component
    ));

    let live = !cli.json && !cli.quiet;
    let sink = ProgressSink::new(live);
    let installation = runtime::install(&client, root, &resolved, 8, &sink).await?;
    sink.finish();

    out.human(format!(
        "installed java {} ({}) at {}",
        installation.major,
        resolved.component,
        installation.home.display()
    ));
    out.json_event(json!({
        "schema": 1,
        "event": "java_installed",
        "major": installation.major,
        "component": resolved.component,
        "version": resolved.version,
    }));

    Ok(())
}

fn remove_java(out: &Out, root: &DataRoot, major: u32) -> MlrcResult<()> {
    let mut pins = Vec::new();
    let versions = root.versions_dir();
    if versions.is_dir() {
        for entry in fs::read_dir(&versions)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(state) =
                InstalledState::read(&entry.path().join(InstalledState::FILE_NAME))?
            {
                if state.java_major == Some(major) {
                    pins.push(state.id);
                }
            }
        }
    }

    if !pins.is_empty() {
        return Err(MlrcError::new(
            ErrorCode::JavaInUse,
            format!("java {major} is still required by: {}", pins.join(", ")),
        ));
    }

    let dir = root.jdk_dir(major);
    if !dir.is_dir() {
        return Err(MlrcError::new(
            ErrorCode::JavaMissing,
            format!("java {major} is not installed"),
        ));
    }
    fs::remove_dir_all(&dir)?;

    out.human(format!("removed java {major}"));
    out.json_event(json!({"schema": 1, "event": "done", "removed_java": major}));
    Ok(())
}

fn read_java_metadata(home: &std::path::Path) -> (String, String) {
    let Ok(text) = fs::read_to_string(home.join("mlrc.json")) else {
        return ("unknown".to_string(), "unknown".to_string());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ("unknown".to_string(), "unknown".to_string());
    };
    let component = value
        .get("component")
        .and_then(|item| item.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version = value
        .get("version")
        .and_then(|item| item.as_str())
        .unwrap_or("unknown")
        .to_string();
    (component, version)
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += dir_size(&entry.path());
        } else if let Ok(metadata) = entry.metadata() {
            total += metadata.len();
        }
    }
    total
}

fn not_implemented(command: &str, milestone: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::NotImplemented,
        format!("`{command}` is not implemented yet (planned in {milestone})"),
    )
}
