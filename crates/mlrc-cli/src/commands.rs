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
    install as run_install, install_installer as run_install_installer,
    install_loader as run_install_loader, preview as run_preview,
    preview_loader as run_preview_loader, resolve_document as run_resolve_document, InstallOptions,
    JavaMode as CoreJavaMode, JavaPolicy, Preview,
};
use mlrc_core::instance::Instance;
use mlrc_core::java::{discover, runtime};
use mlrc_core::launch::{self, LaunchOptions};
use mlrc_core::loader::forge::{self, Flavour};
use mlrc_core::meta::manifest::VersionManifest;
use mlrc_core::meta::rule::Env;
use mlrc_core::meta::version::Artifact;
use mlrc_core::mods;
use mlrc_core::plan::install;
use mlrc_core::port::config::{self, Config, KEYS as CONFIG_KEYS};
use mlrc_core::port::state::{self, InstalledState};
use mlrc_core::port::DataRoot;

use crate::cli::{
    CacheCommand, Cli, Command, ConfigCommand, InstanceCommand, JavaCommand,
    JavaMode as CliJavaMode, MirrorArg, ModCommand,
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
            loader,
        } => {
            cmd_install(
                cli,
                out,
                version,
                *dry_run,
                *threads,
                *java,
                *yes,
                loader.as_deref(),
            )
            .await
        }
        Command::Launch {
            version,
            offline,
            username,
            width,
            height,
            dry_run,
            instance,
            lang,
            memory,
            jvm_arg,
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
                instance.as_deref(),
                lang.as_deref(),
                memory.as_deref(),
                jvm_arg,
                game_args,
            )
            .await
        }
        Command::Uninstall { version } => cmd_uninstall(cli, out, version),
        Command::Cache { command } => cmd_cache(cli, out, command),
        Command::Instance { command } => cmd_instance(cli, out, command).await,
        Command::Mod { command } => cmd_mod(cli, out, command).await,
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

#[allow(clippy::too_many_arguments)]
async fn cmd_install(
    cli: &Cli,
    out: &Out,
    version: &str,
    dry_run: bool,
    threads: usize,
    java_mode: CliJavaMode,
    yes: bool,
    loader: Option<&str>,
) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let (client, source) = open_client(cli, &root).await?;
    announce_source(out, source);

    let (loader_name, loader_version) = parse_loader(loader)?;
    match loader_name {
        Some(name) => out.human(format!("resolving {version} with {name} ...")),
        None => out.human(format!("resolving {version} ...")),
    }

    if dry_run {
        // Installer-based loaders produce their file list while running, so there is
        // nothing to preview beyond the version that would be installed.
        if let Some(name) = loader_name.filter(|name| *name != "fabric") {
            let flavour = Flavour::parse(name).expect("loader names are validated");
            let installer = forge::resolve(&client, flavour, version, loader_version).await?;
            out.human(format!("version      : {}", installer.id));
            out.human(format!("installer    : {}", installer.url));
            out.human("the installer runs at install time; it writes its own file list");
            return Ok(());
        }

        let preview = match loader_name {
            Some(_) => run_preview_loader(&client, &root, version, loader_version).await?,
            None => run_preview(&client, &root, version).await?,
        };
        print_plan(out, &preview, &root);
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

    let state = match install_once(
        &client,
        &root,
        version,
        loader_name,
        loader_version,
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

            install_once(
                &client,
                &root,
                version,
                loader_name,
                loader_version,
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

    // A loader install produces its own id (`1.21.1-forge-52.1.20`), which is what
    // the user has to launch and uninstall; the requested version is not enough.
    let installed_id = state.id.clone();
    out.human(format!(
        "installed {installed_id}: {} files, {}",
        state.files,
        human_bytes(state.bytes)
    ));
    out.json_event(json!({
        "schema": 1,
        "event": "installed",
        "id": installed_id,
        "files": state.files,
        "bytes": state.bytes,
        "java_major": state.java_major,
    }));

    Ok(())
}

const SUPPORTED_LOADERS: &[&str] = &["fabric", "forge", "neoforge"];

/// Splits `fabric` / `fabric:0.16.9` and rejects loaders the engine does not know.
fn parse_loader(spec: Option<&str>) -> MlrcResult<(Option<&str>, Option<&str>)> {
    let Some(spec) = spec else {
        return Ok((None, None));
    };

    let (name, version) = match spec.split_once(':') {
        Some((name, version)) => (name, Some(version)),
        None => (spec, None),
    };

    if !SUPPORTED_LOADERS.contains(&name) {
        return Err(MlrcError::new(
            ErrorCode::LoaderUnavailable,
            format!(
                "unsupported loader: {name} (supported: {})",
                SUPPORTED_LOADERS.join(", ")
            ),
        )
        .with("loader", name.to_string()));
    }

    Ok((Some(name), version))
}

async fn install_once(
    client: &HttpClient,
    root: &DataRoot,
    version: &str,
    loader: Option<&str>,
    loader_version: Option<&str>,
    options: &InstallOptions,
    sink: &ProgressSink,
) -> MlrcResult<InstalledState> {
    match loader {
        Some("fabric") => {
            run_install_loader(client, root, version, loader_version, options, sink).await
        }
        Some(name) => {
            let flavour = Flavour::parse(name).expect("loader names are validated");
            let installer = forge::resolve(client, flavour, version, loader_version).await?;
            run_install_installer(client, root, version, &installer, options, sink).await
        }
        None => run_install(client, root, version, options, sink).await,
    }
}

fn print_plan(out: &Out, preview: &Preview, root: &DataRoot) {
    let plan = &preview.plan;
    let java_installed = plan
        .java
        .as_ref()
        .map(|java| root.jdk_dir(java.major).is_dir())
        .unwrap_or(false);

    out.human(format!(
        "version      : {} ({}, {})",
        preview.id, preview.kind, preview.release_time
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
        "id": preview.id,
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
    instance: Option<&str>,
    lang: Option<&str>,
    memory: Option<&str>,
    jvm_args: &[String],
    game_args: &[String],
) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;

    // No probing here: launch must work without touching the network.
    let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
    let client = HttpClient::new(mirror)?;

    let instance = match instance {
        Some(name) => Some(Instance::load(&root, name)?.ok_or_else(|| {
            MlrcError::new(ErrorCode::Usage, format!("no such instance: {name}"))
                .with("instance", name.to_string())
        })?),
        None => None,
    };

    let id = match version {
        Some(id) => id.to_string(),
        None => match &instance {
            Some(instance) => instance.version.clone(),
            None if !cfg.general.default_version.is_empty() => cfg.general.default_version.clone(),
            None => {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no version given and no default set; run `mlrc use <version>`",
                ))
            }
        },
    };

    let game_dir = match &instance {
        Some(instance) => Instance::game_dir(&root, &instance.name),
        None => root.minecraft_dir(),
    };

    // Heap precedence: flag, then the instance's own value, then the config.
    let heap = match memory {
        Some(value) => Some(launch::memory::normalize(value)?),
        None => instance
            .as_ref()
            .map(|instance| instance.memory.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| Some(cfg.java.xmx.clone()).filter(|value| !value.is_empty())),
    };
    let initial_heap = Some(cfg.java.xms.clone()).filter(|value| !value.is_empty());

    // A flag wins over the configured default; an empty value means "leave the
    // game's own setting alone".
    let language = match lang {
        Some(code) => Some(code.to_string()),
        None => Some(cfg.general.game_language.clone()),
    }
    .filter(|code| !code.is_empty());

    if let Some(code) = &language {
        launch::options::validate_language(code)?;
    }

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

    // Loader versions inherit from vanilla; resolve that before reading javaVersion.
    // Only cached documents are needed after an install, so this works offline.
    let document = run_resolve_document(&client, &root, &id).await?;

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
        jvm_args: jvm_args.to_vec(),
        memory: heap,
        initial_memory: initial_heap,
    };
    let plan = launch::build(
        &document, &plan, &root, &identity, &java, &game_dir, &options,
    )?;

    if dry_run {
        print_launch(out, &plan);
        if let Some(code) = &language {
            out.human(format!("game language would be set to {code}"));
        }
        return Ok(());
    }

    // The game reads its language from options.txt, not from the command line, so
    // this has to happen before the process starts.
    if let Some(code) = &language {
        launch::options::set_language(&game_dir, code)?;
        out.human(format!("game language: {code}"));
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

async fn cmd_instance(cli: &Cli, out: &Out, command: &InstanceCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;

    match command {
        InstanceCommand::Create {
            name,
            version,
            memory,
        } => {
            // An instance only owns its game directory; the version it points at has
            // to exist already.
            match InstalledState::read(&root.version_dir(version).join(InstalledState::FILE_NAME))?
            {
                Some(state) if state.complete => {}
                Some(_) => {
                    return Err(MlrcError::new(
                        ErrorCode::VersionIncomplete,
                        format!("version {version} is incomplete; run `mlrc install {version}`"),
                    ))
                }
                None => {
                    return Err(MlrcError::new(
                        ErrorCode::VersionUnknown,
                        format!("version {version} is not installed; run `mlrc install {version}`"),
                    ))
                }
            }

            if Instance::load(&root, name)?.is_some() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    format!("instance {name} already exists"),
                )
                .with("instance", name.clone()));
            }

            let instance = Instance::create(&root, name, version, memory.as_deref().unwrap_or(""))?;
            let game_dir = Instance::game_dir(&root, &instance.name);

            out.human(format!(
                "created instance {} (version {}, game dir {})",
                instance.name,
                instance.version,
                game_dir.display()
            ));
            out.json_event(json!({
                "schema": 1,
                "event": "instance",
                "name": instance.name,
                "version": instance.version,
                "game_dir": game_dir.display().to_string(),
            }));
        }
        InstanceCommand::Import { file, name, java } => {
            let cfg = Config::load(&root.config_path())?;
            let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
            let client = HttpClient::new(mirror)?;
            let mode = match java {
                CliJavaMode::Auto => CoreJavaMode::Auto,
                CliJavaMode::Install => CoreJavaMode::Install,
                CliJavaMode::Skip => CoreJavaMode::Skip,
            };
            let options = InstallOptions {
                threads: 8,
                java: JavaPolicy {
                    mode,
                    allow_host: cfg.java.custom,
                    host_path: cfg.java.path.clone(),
                },
            };

            let sink = ProgressSink::new(!cli.json && !cli.quiet);
            let summary =
                mods::modpack::import(&client, &root, file, name.as_deref(), &options, &sink)
                    .await?;
            sink.finish();

            out.human(format!(
                "imported {} on {} ({})",
                summary.instance, summary.minecraft, summary.version
            ));
            if let Some(loader) = &summary.loader {
                out.human(format!("loader     : {loader}"));
            }
            out.human(format!(
                "files      : {} installed, {} skipped, {}",
                summary.files,
                summary.skipped,
                human_bytes(summary.bytes)
            ));
            out.json_event(json!({
                "schema": 1,
                "event": "pack_imported",
                "instance": summary.instance,
                "version": summary.version,
                "minecraft": summary.minecraft,
                "loader": summary.loader,
                "files": summary.files,
                "skipped": summary.skipped,
                "bytes": summary.bytes,
            }));
        }
        InstanceCommand::List => {
            let instances = Instance::list(&root)?;

            if out.is_json() {
                let items: Vec<_> = instances
                    .iter()
                    .map(|instance| {
                        json!({
                            "name": instance.name,
                            "version": instance.version,
                            "memory": instance.memory,
                            "game_dir": Instance::game_dir(&root, &instance.name).display().to_string(),
                        })
                    })
                    .collect();
                out.json_event(json!({"schema": 1, "event": "instances", "instances": items}));
                return Ok(());
            }

            if instances.is_empty() {
                out.human("no instances");
                return Ok(());
            }

            out.human(format!(" {:<24}{:<40}{}", "NAME", "VERSION", "HEAP"));
            for instance in &instances {
                let heap = if instance.memory.is_empty() {
                    "-"
                } else {
                    instance.memory.as_str()
                };
                out.human(format!(
                    " {:<24}{:<40}{}",
                    instance.name, instance.version, heap
                ));
            }
        }
        InstanceCommand::Remove { name } => {
            if !Instance::remove(&root, name)? {
                return Err(
                    MlrcError::new(ErrorCode::Usage, format!("no such instance: {name}"))
                        .with("instance", name.clone()),
                );
            }
            out.human(format!("removed instance {name}"));
            out.json_event(json!({"schema": 1, "event": "done", "removed_instance": name}));
        }
    }

    Ok(())
}

async fn cmd_mod(cli: &Cli, out: &Out, command: &ModCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;
    let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
    let client = HttpClient::new(mirror)?;

    match command {
        ModCommand::Search {
            query,
            mc,
            loader,
            instance,
            version,
            source,
            limit,
        } => {
            let source = mods::Source::parse(source)?;
            if source == mods::Source::CurseForge {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "curseforge does not expose search to api keys; open curseforge.com, \
                     find the mod, and install it by numeric project id",
                ));
            }

            let instance = load_instance(&root, instance.as_deref())?;
            let platform = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                mc.as_deref(),
                loader.as_deref(),
            )?;
            let hits =
                mods::modrinth::search(&client, query, &platform.mc, &platform.loader, *limit)
                    .await?;

            if out.is_json() {
                let items: Vec<_> = hits
                    .iter()
                    .map(|hit| {
                        json!({
                            "slug": hit.slug,
                            "title": hit.title,
                            "description": hit.description,
                            "downloads": hit.downloads,
                        })
                    })
                    .collect();
                out.json_event(json!({"schema": 1, "event": "mod_search", "hits": items}));
                return Ok(());
            }

            if hits.is_empty() {
                out.human(format!(
                    "no mods match {query} for minecraft {} on {}",
                    platform.mc, platform.loader
                ));
                return Ok(());
            }

            out.human(format!(" {:<24}{:<34}{}", "SLUG", "TITLE", "DOWNLOADS"));
            for hit in &hits {
                out.human(format!(
                    " {:<24}{:<34}{}",
                    truncate(&hit.slug, 23),
                    truncate(&hit.title, 33),
                    thousands(hit.downloads)
                ));
            }
        }
        ModCommand::Install {
            projects,
            source,
            instance,
            version,
        } => {
            if projects.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no project given; run `mlrc mod install sodium`",
                ));
            }

            let source = mods::Source::parse(source)?;
            let key = cfg.curseforge.key();
            let instance = load_instance(&root, instance.as_deref())?;
            let platform = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = match &instance {
                Some(instance) => Instance::game_dir(&root, &instance.name),
                None => root.minecraft_dir(),
            };

            let sink = ProgressSink::new(!cli.json && !cli.quiet);
            let mut total = 0u64;
            let mut names = Vec::new();

            for project in projects {
                let installed = mods::install_from(
                    &client,
                    &game_dir,
                    &platform,
                    project,
                    source,
                    key.as_deref(),
                    &sink,
                )
                .await?;
                sink.finish();

                for module in &installed {
                    if module.already_present {
                        out.human(format!("already present: {}", module.filename));
                    } else {
                        total += module.bytes;
                        out.human(format!(
                            "installed {} ({})",
                            module.filename,
                            human_bytes(module.bytes)
                        ));
                    }
                    names.push(module.filename.clone());
                }
            }

            out.human(format!(
                "{} file(s) in {}, {} downloaded",
                names.len(),
                mods::mods_dir(&game_dir).display(),
                human_bytes(total)
            ));
            out.json_event(json!({
                "schema": 1,
                "event": "mods_installed",
                "minecraft": platform.mc,
                "loader": platform.loader,
                "files": names,
                "bytes": total,
            }));
        }
        ModCommand::List { instance, version } => {
            let instance = load_instance(&root, instance.as_deref())?;
            let _ = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = game_dir_for(&root, instance.as_ref());
            let dir = mods::mods_dir(&game_dir);

            let manifest = mods::manifest::Manifest::load(&game_dir)?;
            let tracked = manifest.mods.clone();

            let mut files: Vec<String> = Vec::new();
            if dir.is_dir() {
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    if !entry.file_type()?.is_file() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.ends_with(".jar") || name.ends_with(".jar.disabled") {
                        files.push(name);
                    }
                }
            }
            files.sort();

            let untracked: Vec<&String> = files
                .iter()
                .filter(|file| !tracked.iter().any(|entry| &entry.filename == *file))
                .collect();

            if out.is_json() {
                let known: Vec<_> = tracked
                    .iter()
                    .map(|entry| {
                        json!({
                            "project": entry.project,
                            "version": entry.version_number,
                            "filename": entry.filename,
                        })
                    })
                    .collect();
                out.json_event(json!({
                    "schema": 1,
                    "event": "mods",
                    "dir": dir.display().to_string(),
                    "mods": known,
                    "untracked": untracked,
                }));
                return Ok(());
            }

            if tracked.is_empty() && untracked.is_empty() {
                out.human(format!("no mods in {}", dir.display()));
                return Ok(());
            }

            out.human(format!(" {:<24}{:<28}{}", "PROJECT", "VERSION", "FILE"));
            for entry in &tracked {
                let version = if entry.version_number.is_empty() {
                    "-"
                } else {
                    entry.version_number.as_str()
                };
                out.human(format!(
                    " {:<24}{:<28}{}",
                    truncate(&entry.project, 23),
                    truncate(version, 27),
                    entry.filename
                ));
            }
            for file in &untracked {
                out.human(format!(" {:<24}{:<28}{}", "(untracked)", "-", file));
            }
            out.human(format!(
                "{} tracked, {} untracked, in {}",
                tracked.len(),
                untracked.len(),
                dir.display()
            ));
        }
        ModCommand::Update {
            instance,
            version,
            check,
        } => {
            let instance = load_instance(&root, instance.as_deref())?;
            let _ = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = game_dir_for(&root, instance.as_ref());

            let sink = ProgressSink::new(!cli.json && !cli.quiet);
            let report = mods::update(
                &client,
                &game_dir,
                *check,
                cfg.curseforge.key().as_deref(),
                &sink,
            )
            .await?;
            sink.finish();

            if report.outcomes.is_empty() && report.external == 0 {
                out.human("no mods recorded; install some with `mlrc mod install`");
                return Ok(());
            }

            let mut updated = 0usize;
            let mut bytes = 0u64;
            for outcome in &report.outcomes {
                match &outcome.to_version {
                    Some(to) if *check => out.human(format!(
                        "{}: {} -> {} available",
                        outcome.project, outcome.from_version, to
                    )),
                    Some(to) => {
                        updated += 1;
                        bytes += outcome.bytes;
                        out.human(format!(
                            "{}: {} -> {}",
                            outcome.project, outcome.from_version, to
                        ));
                    }
                    None => out.human(format!(
                        "{}: up to date ({})",
                        outcome.project, outcome.from_version
                    )),
                }
            }

            if report.external > 0 {
                out.human(format!(
                    "{} file(s) came from a modpack and are updated with the pack",
                    report.external
                ));
            }
            out.human(format!(
                "{} mod(s) checked, {updated} {}, {} downloaded",
                report.outcomes.len(),
                if *check { "outdated" } else { "updated" },
                human_bytes(bytes)
            ));
            out.json_event(json!({
                "schema": 1,
                "event": "mods_updated",
                "checked": report.outcomes.len(),
                "updated": updated,
                "external": report.external,
                "bytes": bytes,
                "check_only": check,
            }));
        }
        ModCommand::Remove {
            mods: names,
            instance,
            version,
        } => {
            let instance = load_instance(&root, instance.as_deref())?;
            let _ = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = game_dir_for(&root, instance.as_ref());

            if names.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no mod given; run `mlrc mod remove sodium`",
                ));
            }

            let removed = mods::remove(&game_dir, names)?;
            for item in &removed {
                out.human(format!("removed {} ({})", item.project, item.filename));
            }
            let files: Vec<&str> = removed.iter().map(|item| item.filename.as_str()).collect();
            out.json_event(json!({
                "schema": 1,
                "event": "mods_removed",
                "files": files,
            }));
        }
        ModCommand::Disable {
            mods: names,
            instance,
            version,
        }
        | ModCommand::Enable {
            mods: names,
            instance,
            version,
        } => {
            let enabled = matches!(command, ModCommand::Enable { .. });
            let instance = load_instance(&root, instance.as_deref())?;
            let _ = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = game_dir_for(&root, instance.as_ref());

            if names.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    format!(
                        "no mod given; run `mlrc mod {} sodium`",
                        if enabled { "enable" } else { "disable" }
                    ),
                ));
            }

            let toggled = mods::set_enabled(&game_dir, names, enabled)?;
            for item in &toggled {
                if item.changed {
                    out.human(format!(
                        "{} {} -> {}",
                        if item.enabled { "enabled" } else { "disabled" },
                        item.project,
                        item.filename
                    ));
                } else {
                    out.human(format!(
                        "{} already {}",
                        item.project,
                        if enabled { "enabled" } else { "disabled" }
                    ));
                }
            }

            let changed: Vec<&str> = toggled
                .iter()
                .filter(|item| item.changed)
                .map(|item| item.filename.as_str())
                .collect();
            out.json_event(json!({
                "schema": 1,
                "event": "mods_toggled",
                "enabled": enabled,
                "files": changed,
            }));
        }
    }

    Ok(())
}

fn game_dir_for(root: &DataRoot, instance: Option<&Instance>) -> std::path::PathBuf {
    match instance {
        Some(instance) => Instance::game_dir(root, &instance.name),
        None => root.minecraft_dir(),
    }
}

/// Resolves the loader and Minecraft version to search or install against.
///
/// Explicit `--mc`/`--loader` win; otherwise they come from the target version, which
/// is the instance's, the `--version` one, or the configured default.
fn mod_platform(
    root: &DataRoot,
    cfg: &Config,
    version: Option<&str>,
    instance: Option<&Instance>,
    mc: Option<&str>,
    loader: Option<&str>,
) -> MlrcResult<mods::Platform> {
    let mut platform = match (mc, loader) {
        (Some(mc), Some(loader)) => mods::Platform {
            mc: mc.to_string(),
            loader: loader.to_string(),
        },
        _ => {
            let id = match version {
                Some(id) => id.to_string(),
                None => match instance {
                    Some(instance) => instance.version.clone(),
                    None if !cfg.general.default_version.is_empty() => {
                        cfg.general.default_version.clone()
                    }
                    None => {
                        return Err(MlrcError::new(
                            ErrorCode::Usage,
                            "no version to take the loader from; pass --mc and --loader, or `mlrc use <version>`",
                        ))
                    }
                },
            };
            mods::platform(root, &id)?
        }
    };

    if let Some(mc) = mc {
        platform.mc = mc.to_string();
    }
    if let Some(loader) = loader {
        platform.loader = loader.to_string();
    }

    if !["fabric", "forge", "neoforge"].contains(&platform.loader.as_str()) {
        return Err(MlrcError::new(
            ErrorCode::Usage,
            format!(
                "unknown loader {} (expected fabric, forge or neoforge)",
                platform.loader
            ),
        ));
    }

    Ok(platform)
}

fn load_instance(root: &DataRoot, name: Option<&str>) -> MlrcResult<Option<Instance>> {
    let Some(name) = name else {
        return Ok(None);
    };
    Instance::load(root, name)?.map(Some).ok_or_else(|| {
        MlrcError::new(ErrorCode::Usage, format!("no such instance: {name}"))
            .with("instance", name.to_string())
    })
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('~');
    out
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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
