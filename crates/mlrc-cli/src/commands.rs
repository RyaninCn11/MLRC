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
use mlrc_core::loader::fabric::{self as fabric, Family};
use mlrc_core::loader::forge::{self, Flavour};
use mlrc_core::meta::manifest::{Channel, VersionManifest};
use mlrc_core::meta::rule::Env;
use mlrc_core::meta::version::Artifact;
use mlrc_core::mods;
use mlrc_core::plan::install;
use mlrc_core::port::config::{self, Config, LaunchConfig, KEYS as CONFIG_KEYS};
use mlrc_core::port::state::{self, InstalledState};
use mlrc_core::port::DataRoot;

use crate::cli::{
    AuthCommand, CacheCommand, Cli, Command, ConfigCommand, InstanceCommand, JavaCommand,
    JavaMode as CliJavaMode, MirrorArg, ModCommand, PackCommand,
};
use crate::progress::Sink;
use crate::render::Out;

pub async fn run(cli: &Cli, out: &Out) -> MlrcResult<()> {
    match &cli.command {
        Command::Doctor => cmd_doctor(cli, out).await,
        Command::Config { command } => cmd_config(cli, out, command),
        Command::Use { version } => cmd_use(cli, out, version),
        Command::List { all } => cmd_list(cli, out, *all).await,
        Command::Install {
            version,
            list,
            kind,
            limit,
            dry_run,
            threads,
            java,
            yes,
            loader,
        } => {
            if *list {
                install_list(cli, out, loader.as_deref(), kind.as_deref(), *limit).await
            } else {
                cmd_install(
                    cli,
                    out,
                    version.as_deref().unwrap_or_default(),
                    *dry_run,
                    *threads,
                    *java,
                    *yes,
                    loader.as_deref(),
                )
                .await
            }
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
        Command::Pack { command } => cmd_pack(cli, out, command).await,
        Command::Java { command } => cmd_java(cli, out, command).await,
        Command::Auth { command } => cmd_auth(cli, out, command).await,
    }
}

async fn cmd_auth(cli: &Cli, out: &Out, command: &AuthCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let mut cfg = Config::load(&root.config_path())?;

    match command {
        AuthCommand::Login {
            server,
            nide8,
            username,
            password_stdin,
            name,
        } => {
            // Unified pass is the same protocol behind a discoverable base url, so the
            // only difference here is where that base comes from.
            let nide8_id = nide8
                .clone()
                .or_else(|| Some(cfg.auth.nide8_id.clone()).filter(|value| !value.is_empty()));
            if let Some(id) = &nide8_id {
                auth::nide8::validate_id(id)?;
            }

            let server = match (&nide8_id, server) {
                (None, Some(server)) => Some(auth::validate_server(server)?),
                (None, None) => {
                    let configured = cfg.auth.server.clone();
                    if configured.is_empty() {
                        None
                    } else {
                        Some(auth::validate_server(&configured)?)
                    }
                }
                (Some(_), _) => None,
            };
            if nide8_id.is_none() && server.is_none() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no authentication server; pass --server or --nide8, or set one with `mlrc config set`",
                ));
            }
            let username = match username {
                Some(name) => name.clone(),
                None => prompt_line("username: ")?,
            };
            if username.trim().is_empty() {
                return Err(MlrcError::new(ErrorCode::Usage, "no username given"));
            }

            let password = if *password_stdin {
                read_stdin_line()?
            } else {
                rpassword::prompt_password("password: ").map_err(|err| {
                    MlrcError::new(ErrorCode::Io, format!("cannot read the password: {err}"))
                })?
            };

            let (client, mirror) = open_client(cli, &root).await?;

            // The unified pass base url comes from the server document, so discovery
            // happens once the client exists.
            let mut unified = None;
            let server = match &nide8_id {
                Some(id) => {
                    let discovered = auth::nide8::discover(&client, id).await?;
                    out.human(format!(
                        "unified pass: {} ({})",
                        if discovered.name.is_empty() {
                            "unnamed server"
                        } else {
                            discovered.name.as_str()
                        },
                        if discovered.ip.is_empty() {
                            "no address"
                        } else {
                            discovered.ip.as_str()
                        }
                    ));
                    let root_url = discovered.api_root.clone();
                    unified = Some(discovered);
                    root_url
                }
                None => server.expect("checked before the prompt"),
            };

            let mut session =
                auth::yggdrasil::authenticate(&client, &server, username.trim(), &password, None)
                    .await?;
            if let Some(discovered) = &unified {
                session.kind = "nide8".to_string();
                session.server_id = discovered.id.clone();
            }

            let mut accounts = auth::Accounts::load(&root)?;
            let label = accounts.put(name.as_deref(), session.clone());
            accounts.save(&root)?;
            let _ = auth::yggdrasil::Session::remove(&root);

            // Record the backend and where it came from, so a launch does not have to
            // infer either from which keys happen to be set.
            let wanted = if unified.is_some() {
                config::AuthKind::Nide8
            } else {
                config::AuthKind::Yggdrasil
            };
            let before = (
                cfg.auth.kind,
                cfg.auth.nide8_id.clone(),
                cfg.auth.server.clone(),
            );
            cfg.auth.kind = wanted;
            match &unified {
                Some(discovered) => cfg.auth.nide8_id = discovered.id.clone(),
                None => cfg.auth.server = server.clone(),
            }
            if (
                cfg.auth.kind,
                cfg.auth.nide8_id.clone(),
                cfg.auth.server.clone(),
            ) != before
            {
                cfg.save(&root.config_path())?;
            }

            // The client component is offered here, right after logging in, because that
            // is the moment the user decided to use this backend.
            let injector = match &unified {
                Some(server) => {
                    ensure_nide8_jar(cli, out, &root, &cfg, &client, Some(server)).await?;
                    None
                }
                None => Some(auth::yggdrasil::ensure_injector(&client, &root, mirror).await?),
            };

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "done",
                    "account": {
                        "server": session.server,
                        "name": session.name,
                        "uuid": session.uuid,
                        "kind": if session.kind.is_empty() { "yggdrasil" } else { session.kind.as_str() },
                    },
                    "label": label,
                    "injector": injector.as_ref().map(|injector| injector.build),
                }));
            } else {
                out.human(format!(
                    "signed in as {} ({}) on {}",
                    session.name, session.uuid, session.server
                ));
                out.human(format!("account:  {label}"));
                match &injector {
                    Some(injector) => {
                        out.human(format!("authlib-injector build {}", injector.build))
                    }
                    None => out.human("unified pass: nide8auth.jar found"),
                }
            }
            Ok(())
        }
        AuthCommand::Register { nide8, print_only } => {
            let id = nide8
                .clone()
                .or_else(|| Some(cfg.auth.nide8_id.clone()).filter(|value| !value.is_empty()))
                .ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::Usage,
                        "no unified pass server; pass --nide8 <id> or `mlrc config set auth.nide8_id <id>`",
                    )
                })?;
            let url = auth::nide8::register_url(auth::nide8::validate_id(&id)?);

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "register",
                    "url": url,
                }));
            } else {
                out.human(format!("register: {url}"));
            }

            if !print_only {
                match open_in_browser(&url) {
                    Ok(()) => {}
                    Err(error) => out.human(format!(
                        "could not open a browser ({error}); open the url above"
                    )),
                }
            }
            Ok(())
        }
        AuthCommand::Logout { name } => {
            let mut accounts = auth::Accounts::load(&root)?;
            let label = match name {
                Some(label) => label.clone(),
                None => accounts.default.clone(),
            };

            let session = accounts.get(&label).map(|account| account.session.clone());
            if let Some(session) = session {
                let (client, _) = open_client(cli, &root).await?;
                // Best effort: the entry goes away whether or not the server answers,
                // which is what the user asked for.
                let _ = auth::yggdrasil::invalidate(&client, &session).await;
            }

            let removed = accounts.remove(&label);
            if removed {
                accounts.save(&root)?;
            }

            // No account means no backend to pin; auto falls back to offline.
            if accounts.accounts.is_empty() && cfg.auth.kind != config::AuthKind::Auto {
                cfg.auth.kind = config::AuthKind::Auto;
                cfg.save(&root.config_path())?;
            }

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "done",
                    "logged_out": removed,
                }));
            } else if removed {
                out.human("signed out");
            } else {
                out.human("no account stored");
            }
            Ok(())
        }
        AuthCommand::Accounts => {
            let accounts = auth::Accounts::load(&root)?;
            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "accounts",
                    "default": accounts.default,
                    "accounts": accounts.labels(),
                }));
                return Ok(());
            }

            if accounts.accounts.is_empty() {
                out.human("no accounts; run `mlrc auth login`");
                return Ok(());
            }
            for account in &accounts.accounts {
                let marker = if account.label.eq_ignore_ascii_case(&accounts.default) {
                    "*"
                } else {
                    " "
                };
                let kind = if account.session.kind.is_empty() {
                    "yggdrasil"
                } else {
                    account.session.kind.as_str()
                };
                out.human(format!(
                    "{marker} {:<16} {:<10} {}",
                    account.label, kind, account.session.name
                ));
            }
            Ok(())
        }
        AuthCommand::Use { label } => {
            let mut accounts = auth::Accounts::load(&root)?;
            accounts.set_default(label)?;
            accounts.save(&root)?;
            out.human(format!("default account: {}", accounts.default));
            out.json_event(json!({
                "schema": 1,
                "event": "account",
                "default": accounts.default,
            }));
            Ok(())
        }
        AuthCommand::Status { instance } => {
            let accounts = auth::Accounts::load(&root)?;
            let fork = match instance {
                Some(name) => Some(Instance::load(&root, name)?.ok_or_else(|| {
                    MlrcError::new(ErrorCode::Usage, format!("no such instance: {name}"))
                        .with("instance", name.clone())
                })?),
                None => None,
            };
            let session = accounts
                .get(&accounts.default)
                .map(|account| account.session.clone());
            let stored_kind = session
                .as_ref()
                .map(|session| session.kind.as_str())
                .filter(|kind| !kind.is_empty());
            let resolved = cfg.auth.kind.resolve(stored_kind);

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "account",
                    "mode": cfg.auth.kind.as_str(),
                    "backend": resolved.backend(),
                    "ready": resolved.is_ready(),
                    "account": session.as_ref().map(|session| json!({
                        "kind": stored_kind.unwrap_or("yggdrasil"),
                        "server": session.server,
                        "server_id": session.server_id,
                        "username": session.username,
                        "name": session.name,
                        "uuid": session.uuid,
                    })),
                    "client": nide8_client_state(&root, &cfg),
                }));
                return Ok(());
            }

            // What a launch would do, and why: the mode answers "how does it decide who
            // I am", and the account block is the stored half of that answer.
            if let Some(instance) = &fork {
                let wanted = instance.fork.auth.account.trim();
                match accounts.resolve(wanted) {
                    Ok(account) => out.human(format!(
                        "instance: {} uses {}",
                        instance.name, account.label
                    )),
                    Err(_) if wanted.is_empty() => out.human(format!(
                        "instance: {} uses the default account",
                        instance.name
                    )),
                    Err(err) => out.human(format!(
                        "instance: {} names {} ({})",
                        instance.name, wanted, err.message
                    )),
                }
            }

            match &resolved {
                config::Resolved::Offline => {
                    out.human(format!(
                        "mode:     offline (auth.kind = {})",
                        cfg.auth.kind.as_str()
                    ));
                    match &session {
                        Some(_) => out.human(
                            "account:  stored but unused; `mlrc config set auth.kind auto` uses it",
                        ),
                        None => out.human("account:  none"),
                    }
                    out.human(format!("name:     {}", cfg.auth.player_name));
                }
                config::Resolved::Missing(backend) => {
                    out.human(format!(
                        "mode:     {backend} (auth.kind = {}) but no account is stored",
                        cfg.auth.kind.as_str()
                    ));
                    out.human("account:  none; run `mlrc auth login`");
                }
                config::Resolved::Wrong(backend) => {
                    out.human(format!(
                        "mode:     {backend} (auth.kind = {}) but the stored account is {}",
                        cfg.auth.kind.as_str(),
                        stored_kind.unwrap_or("yggdrasil")
                    ));
                    out.human("account:  run `mlrc auth login` again, or set auth.kind to auto");
                }
                _ => {
                    let session = session.as_ref().expect("a ready backend has a session");
                    let kind = stored_kind.unwrap_or("yggdrasil");
                    out.human(format!(
                        "mode:     {} (auth.kind = {})",
                        kind,
                        cfg.auth.kind.as_str()
                    ));
                    out.human(format!("account:  {}", session.name));
                    out.human(format!("uuid:     {}", session.uuid));
                    out.human(format!("server:   {}", session.server));
                    if !session.server_id.is_empty() {
                        out.human(format!("server id: {}", session.server_id));
                    }
                    out.human(format!("login:    {}", session.username));

                    match kind {
                        "nide8" => {
                            out.human(format!("client:   {}", nide8_client_state(&root, &cfg)));
                        }
                        _ => match auth::yggdrasil::cached_injector(&root)? {
                            Some(injector) => {
                                out.human(format!("injector: build {}", injector.build));
                            }
                            None => out.human("injector: not downloaded"),
                        },
                    }
                }
            }
            Ok(())
        }
    }
}

fn prompt_line(label: &str) -> MlrcResult<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    read_stdin_line()
}

fn read_stdin_line() -> MlrcResult<String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| MlrcError::new(ErrorCode::Io, format!("cannot read stdin: {err}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

async fn cmd_doctor(cli: &Cli, out: &Out) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;
    let (client, _) = open_client(cli, &root).await?;
    let checks = doc::run(&root, &cfg, &client).await;
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();

    if out.is_json() {
        // One event for the whole report: a consumer should not have to reassemble the
        // picture from a stream of individual checks.
        let items: Vec<_> = checks
            .iter()
            .map(|check| {
                json!({
                    "name": check.name,
                    "status": check.status.as_str(),
                    "detail": check.detail,
                })
            })
            .collect();
        out.json_event(json!({
            "schema": 1,
            "event": "doctor",
            "ok": failed == 0,
            "failed": failed,
            "warned": warned,
            "checks": items,
        }));
    } else {
        for check in &checks {
            out.human(format!(
                "{:<14}{}",
                format!("{}:", check.name),
                check.detail
            ));
        }
    }

    if failed > 0 {
        return Err(MlrcError::new(
            ErrorCode::Doctor,
            format!("{failed} check(s) failed"),
        ));
    }

    Ok(())
}

fn cmd_config(cli: &Cli, out: &Out, command: &ConfigCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    let path = root.config_path();

    // A fork stores the launch.* keys and nothing else, so a command that names one
    // writes there; everything else is global by definition.
    let fork = match command {
        ConfigCommand::List { instance }
        | ConfigCommand::Get { instance, .. }
        | ConfigCommand::Set { instance, .. } => match instance {
            Some(name) => {
                root.ensure()?;
                Some(Instance::load(&root, name)?.ok_or_else(|| {
                    MlrcError::new(
                        ErrorCode::VersionUnknown,
                        format!("no such instance: {name}"),
                    )
                    .with("instance", name.clone())
                })?)
            }
            None => None,
        },
    };

    match command {
        ConfigCommand::List { .. } => {
            let cfg = Config::load(&path)?;
            if !out.is_json() {
                match &fork {
                    Some(instance) => out.human(format!(
                        "# fork {} (instance.json); empty means the global value",
                        instance.name
                    )),
                    None => out.human(format!("# global: {}", path.display())),
                }
                out.human("# a fork may override the launch.* keys in its own instance.json");
            }

            let mut keys: Vec<&str> = CONFIG_KEYS.to_vec();
            if fork.is_some() {
                keys.push("auth.account");
            }

            for key in keys {
                let global = if config::is_fork_only(key) {
                    String::new()
                } else {
                    cfg.get(key)?
                };
                let (value, source) = match &fork {
                    Some(instance) if config::is_fork_key(key) => {
                        match instance.fork.overridden(key) {
                            Some(value) => (value, "fork"),
                            None => (global, "global"),
                        }
                    }
                    _ => (global, "global"),
                };

                if out.is_json() {
                    out.json_event(json!({
                        "schema": 1,
                        "event": "config",
                        "key": key,
                        "value": value,
                        "source": source,
                    }));
                } else {
                    let scope = match (&fork, source) {
                        (None, _) if config::is_fork_key(key) => String::new(),
                        (None, _) => " (global only)".to_string(),
                        (Some(_), source) => format!(" [{source}]"),
                    };
                    out.human(format!("{key} = {value}{scope}"));
                }
            }
            Ok(())
        }
        ConfigCommand::Get { key, .. } => {
            let cfg = Config::load(&path)?;
            let canonical = config::canonical(key)?;
            if fork.is_none() && config::is_fork_only(canonical) {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    format!("{canonical} only exists per fork; pass --instance <name>"),
                )
                .with("key", canonical.to_string()));
            }
            let global = if config::is_fork_only(canonical) {
                String::new()
            } else {
                cfg.get(key)?
            };
            let (value, source) = match &fork {
                Some(instance) if config::is_fork_key(key) => match instance.fork.overridden(key) {
                    Some(value) => (value, "fork"),
                    None => (global, "global"),
                },
                _ => (global, "global"),
            };

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "config",
                    "key": key,
                    "value": value,
                    "source": source,
                }));
            } else {
                out.human(value);
                if fork.is_some() && key.starts_with("launch.") {
                    out.human(format!("# from {source}"));
                }
            }
            Ok(())
        }
        ConfigCommand::Set { key, value, .. } => {
            let canonical = config::canonical(key)?;

            if let Some(mut instance) = fork {
                if !config::is_fork_key(canonical) {
                    return Err(MlrcError::new(
                        ErrorCode::Usage,
                        format!(
                            "{canonical} is global; only the launch.* keys can be set per fork"
                        ),
                    )
                    .with("key", canonical.to_string()));
                }
                instance.fork.set_key(canonical, value)?;
                instance.save(&root)?;
                let stored = instance.fork.get_key(canonical)?;
                out.human(format!("{}: {key} = {stored}", instance.name));
                out.json_event(json!({
                    "schema": 1,
                    "event": "config",
                    "key": key,
                    "value": stored,
                    "source": "fork",
                    "instance": instance.name,
                }));
                return Ok(());
            }

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

    match InstalledState::load(&root, version)? {
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
    // `--all` is the same list `install --list` prints; one implementation, so the two
    // cannot drift apart.
    if all {
        return install_list(cli, out, None, None, None).await;
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
            let state = InstalledState::load(&root, &id)?;
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

/// Makes a stored token usable before the game starts.
///
/// One request decides: a live token is used as it is, a rejected one is refreshed and
/// the new one written back, and an unreachable server means the launch continues with
/// what is on disk - playing alone is the common case, and a dead network must not
/// block it. The token is never printed; only the account name is.
async fn refresh_session(
    cli: &Cli,
    root: &DataRoot,
    cfg: &Config,
    session: auth::yggdrasil::Session,
    out: &Out,
) -> MlrcResult<auth::yggdrasil::Session> {
    let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
    let client = HttpClient::with_proxy(mirror, &cfg.general.proxy)?;

    // The service publishes its client component and the hash of it, so a missing or
    // stale jar is handled here, before the game starts and fails for no visible reason.
    if session.kind == "nide8" {
        let server = auth::nide8::discover(&client, &session.server_id)
            .await
            .ok();
        ensure_nide8_jar(cli, out, root, cfg, &client, server.as_ref()).await?;
    }

    match auth::yggdrasil::validate(&client, &session).await? {
        Some(true) => Ok(session),
        Some(false) => {
            let fresh = auth::yggdrasil::refresh(&client, &session)
                .await
                .map_err(|err| {
                    MlrcError::new(err.code, err.message.clone()).with(
                        "hint",
                        "run `mlrc auth login` to sign in again, or `--username` to play offline",
                    )
                })?;
            fresh.save(root)?;
            out.human(format!("session refreshed for {}", fresh.name));
            Ok(fresh)
        }
        None => {
            out.human("cannot reach the authentication server; using the stored session");
            Ok(session)
        }
    }
}
/// A human at the keyboard, not a pipe and not a machine-readable run.
fn interactive(cli: &Cli, out: &Out) -> bool {
    use std::io::IsTerminal;
    !out.is_json()
        && !cli.quiet
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
}

/// How many of `total` to print. Enter takes the offered ten, `all` takes everything,
/// and a number is a number.
fn ask_how_many(total: usize) -> MlrcResult<usize> {
    let offered = 10.min(total);
    print!("show the first ({offered}/{total}) - Enter for {offered}, a number, or `all`: ");
    std::io::stdout().flush()?;

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| MlrcError::new(ErrorCode::Io, format!("cannot read stdin: {err}")))?;

    parse_count(line.trim(), total)
}

fn parse_count(answer: &str, total: usize) -> MlrcResult<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(10.min(total));
    }
    if answer.eq_ignore_ascii_case("all") || answer == "*" {
        return Ok(total);
    }

    match answer.parse::<usize>() {
        Ok(0) => Ok(total),
        Ok(count) => Ok(count.min(total)),
        Err(_) => Err(MlrcError::new(
            ErrorCode::Usage,
            format!("expected a number or `all`, got {answer}"),
        )),
    }
}

/// `mlrc install --list`: what can be installed, optionally only what a loader has a
/// build for, optionally only one category.
///
/// Mods are deliberately not part of this: there is no bounded list to show, and
/// `mlrc mod search` is the question people actually have.
async fn install_list(
    cli: &Cli,
    out: &Out,
    loader: Option<&str>,
    kind: Option<&str>,
    limit: Option<usize>,
) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let (client, source) = open_client(cli, &root).await?;
    announce_source(out, source);
    let manifest = VersionManifest::load(&client, &root).await?;

    let (loader_name, _) = parse_loader(loader)?;
    let channel = match kind {
        Some(text) => Some(Channel::parse(text).ok_or_else(|| {
            let known: Vec<&str> = Channel::ALL.iter().map(|item| item.as_str()).collect();
            MlrcError::new(
                ErrorCode::Usage,
                format!("unknown type {text} (expected {})", known.join(", ")),
            )
            .with("type", text.to_string())
        })?),
        None => None,
    };

    let supported: Option<BTreeSet<String>> = match loader_name {
        None => None,
        Some(name) => Some(match Family::parse(name) {
            Some(family) => fabric::supported_game_versions(&client, family)
                .await?
                .into_iter()
                .collect(),
            None => {
                let flavour = Flavour::parse(name).expect("parse_loader checked the name");
                let known: Vec<String> = manifest
                    .versions
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect();
                forge::supported_minecraft_versions(&client, flavour, &known)
                    .await?
                    .into_iter()
                    .collect()
            }
        }),
    };

    let versions: Vec<_> = manifest
        .versions
        .iter()
        .filter(|entry| match &supported {
            Some(ids) => ids.contains(&entry.id),
            None => true,
        })
        .filter(|entry| match channel {
            Some(wanted) => entry.channel() == wanted,
            None => true,
        })
        .collect();

    let mut label: Vec<String> = Vec::new();
    if let Some(name) = loader_name {
        label.push(name.to_string());
    }
    if let Some(channel) = channel {
        label.push(channel.as_str().to_string());
    }

    if label.is_empty() {
        out.human(format!(
            "{} versions; latest release {}, latest snapshot {}",
            versions.len(),
            manifest.latest.release,
            manifest.latest.snapshot
        ));
    } else {
        out.human(format!(
            "{}: {} of {} versions; latest release {}, latest snapshot {}",
            label.join(", "),
            versions.len(),
            manifest.versions.len(),
            manifest.latest.release,
            manifest.latest.snapshot
        ));
    }

    // The whole point of the list is that it is long, so an interactive run shows a
    // slice and asks; a pipe, `--json` and `--limit` are deterministic.
    let total = versions.len();
    let shown = match limit {
        Some(0) => total,
        Some(count) => count.min(total),
        None if interactive(cli, out) => ask_how_many(total)?,
        None => total,
    };

    out.human(format!("{:<26}{:<16}{}", "ID", "TYPE", "RELEASED"));

    for version in versions.iter().take(shown) {
        out.human(format!(
            "{:<26}{:<16}{}",
            version.id,
            version.channel().as_str(),
            version.release_time
        ));

        let mut event = json!({
            "schema": 1,
            "event": "version",
            "id": version.id,
            "type": version.kind,
            "channel": version.channel().as_str(),
            "release_time": version.release_time,
        });
        if let Some(name) = loader_name {
            event["loader"] = json!(name);
        }
        out.json_event(event);
    }

    if shown < total {
        out.human(format!(
            "... {} more; `mlrc install --list --limit 0` prints every one",
            total - shown
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
        if let Some(name) = loader_name.filter(|name| Flavour::parse(name).is_some()) {
            let flavour = Flavour::parse(name).expect("checked by the filter");
            let installer = forge::resolve(&client, flavour, version, loader_version).await?;
            out.human(format!("version      : {}", installer.id));
            out.human(format!("installer    : {}", installer.url));
            out.human("the installer runs at install time; it writes its own file list");
            return Ok(());
        }

        let preview = match loader_name {
            Some(name) => {
                let family = family_for(name);
                run_preview_loader(&client, &root, version, family, loader_version).await?
            }
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

    let sink = Sink::for_run(cli.json, cli.quiet);

    let state = match install_once(
        &client,
        &root,
        version,
        loader_name,
        loader_version,
        &InstallOptions {
            threads,
            java: policy.clone(),
            proxy: cfg.general.proxy.clone(),
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

            // Machine callers get a decision request, never a prompt. In JSON mode the
            // sink has already emitted `java_required` (with `download_bytes`), so only
            // the non-JSON machine case needs a line here.
            if cli.json || !std::io::stdin().is_terminal() {
                if !cli.json {
                    out.json_event(json!({
                        "schema": 1,
                        "event": "java_required",
                        "component": component,
                        "major": major,
                    }));
                }
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
                    proxy: cfg.general.proxy.clone(),
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

const SUPPORTED_LOADERS: &[&str] = &["fabric", "quilt", "forge", "neoforge"];

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
    sink: &Sink,
) -> MlrcResult<InstalledState> {
    match loader {
        Some(name) if Flavour::parse(name).is_some() => {
            let flavour = Flavour::parse(name).expect("checked by the guard");
            let installer = forge::resolve(client, flavour, version, loader_version).await?;
            run_install_installer(client, root, version, &installer, options, sink).await
        }
        Some(name) => {
            let family = family_for(name);
            run_install_loader(client, root, version, family, loader_version, options, sink).await
        }
        None => run_install(client, root, version, options, sink).await,
    }
}

/// The meta-API loaders share one code path; anything else is an installer.
fn family_for(name: &str) -> Family {
    Family::parse(name).unwrap_or(Family::Fabric)
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

    Ok((
        HttpClient::with_proxy(resolved, &cfg.general.proxy)?,
        resolved,
    ))
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
    offline: bool,
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
    let client = HttpClient::with_proxy(mirror, &cfg.general.proxy)?;

    let instance = match instance {
        Some(name) => Some(Instance::load(&root, name)?.ok_or_else(|| {
            MlrcError::new(ErrorCode::Usage, format!("no such instance: {name}"))
                .with("instance", name.to_string())
        })?),
        None => None,
    };

    // Which layers decide: a fork may name its own account, Java settings and launch
    // options; everything it leaves empty comes from the global file.
    let fork_layers = match &instance {
        Some(instance) => instance.fork.clone(),
        None => config::ForkConfig::default(),
    };
    let effective = fork_layers.apply(&cfg);

    let id = match version {
        Some(id) => id.to_string(),
        None => match &instance {
            // A fork is itself a version id; its document carries the inheritance.
            Some(instance) => instance.name.clone(),
            None if !cfg.general.default_version.is_empty() => cfg.general.default_version.clone(),
            None => {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no version given and no default set; run `mlrc use <version>`",
                ))
            }
        },
    };

    // Every version runs in its own directory, which is also where its saves and mods
    // live; `--instance` is just shorthand for naming that version.
    let game_dir = match &instance {
        Some(instance) => Instance::game_dir(&root, &instance.name),
        None => root.game_dir(&id),
    };

    // Heap precedence: flag, then the instance's own value, then the config.
    let heap = match memory {
        Some(value) => Some(launch::memory::normalize(value)?),
        None => instance
            .as_ref()
            .map(|instance| instance.fork.java.xmx.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| Some(effective.java.xmx.clone()).filter(|value| !value.is_empty())),
    };
    let initial_heap = Some(effective.java.xms.clone()).filter(|value| !value.is_empty());

    // A flag wins over the configured default; an empty value means "leave the
    // game's own setting alone".
    let language = match lang {
        Some(code) => Some(code.to_string()),
        None => Some(effective.launch.game_language.clone()),
    }
    .filter(|code| !code.is_empty());

    if let Some(code) = &language {
        launch::options::validate_language(code)?;
    }

    match InstalledState::load(&root, &id)? {
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
    launch::natives::extract(&plan, &root, &id)?;

    // `--offline` and `--username` both mean "do not use a stored account"; otherwise
    // `auth.kind` decides, and the account it selects says which backend it belongs to.
    let force_offline = offline || username.is_some();
    let accounts = auth::Accounts::load(&root)?;

    let account = if force_offline || cfg.auth.kind == config::AuthKind::Offline {
        None
    } else {
        match accounts.resolve(&effective.account) {
            Ok(account) => Some(account.clone()),
            // `auto` with nothing configured is simply offline.
            Err(_) if cfg.auth.kind == config::AuthKind::Auto && effective.account.is_empty() => {
                None
            }
            Err(err) => return Err(err),
        }
    };

    let resolved = if force_offline {
        config::Resolved::Offline
    } else {
        cfg.auth.kind.resolve(
            account
                .as_ref()
                .map(|account| account.session.kind.as_str())
                .filter(|kind| !kind.is_empty()),
        )
    };

    let session =
        match resolved {
            config::Resolved::Offline => None,
            config::Resolved::Missing(backend) => {
                return Err(MlrcError::new(
                    ErrorCode::AuthFailed,
                    format!("auth.kind is {backend} but no account is stored"),
                )
                .with(
                    "hint",
                    "run `mlrc auth login`, or `mlrc config set auth.kind auto`",
                ))
            }
            config::Resolved::Wrong(backend) => return Err(MlrcError::new(
                ErrorCode::AuthFailed,
                format!("auth.kind is {backend} but the selected account is something else"),
            )
            .with(
                "hint",
                "name another account with `mlrc config set auth.account`, or set auth.kind auto",
            )),
            _ => account.map(|account| account.session),
        };

    let session = match session {
        Some(session) => Some(refresh_session(cli, &root, &cfg, session, out).await?),
        None => None,
    };
    let mut jvm_args = jvm_args.to_vec();
    let identity = match &session {
        Some(session) if session.kind == "nide8" => {
            // Unified Pass needs two things the other backends do not: its own client
            // component on the JVM, and Java 8u101 or newer, below which that component
            // cannot hook the game.
            if java.major == 8 {
                let update = discover::read_release_update(&java.home).unwrap_or(0);
                if update < 101 {
                    return Err(MlrcError::new(
                        ErrorCode::JavaTooOld,
                        format!(
                            "unified pass needs java 8u101 or newer; this runtime is 8u{update}"
                        ),
                    )
                    .with("hint", "install a newer runtime with `mlrc java install 8`"));
                }
            }

            let jar = nide8_jar(&root, &cfg)?;
            jvm_args.extend(auth::nide8::java_arguments(
                &jar,
                &session.server_id,
                &game_dir,
            ));
            session.identity()
        }
        Some(session) => {
            let injector = auth::yggdrasil::cached_injector(&root)?.ok_or_else(|| {
                MlrcError::new(
                    ErrorCode::AuthFailed,
                    "authlib-injector is missing; run `mlrc auth login` again",
                )
            })?;
            jvm_args.push(injector.argument(&session.server));
            session.identity()
        }
        None => {
            let name = match username {
                Some(name) => name.to_string(),
                None if !cfg.auth.player_name.trim().is_empty() => {
                    cfg.auth.player_name.trim().to_string()
                }
                None => "Player".to_string(),
            };
            auth::offline::validate_name(&name)?;
            auth::offline(&name)
        }
    };

    // Global launch settings, then the fork's own values over them, then whatever the
    // command line added on top.
    let launch_cfg = effective.launch.clone();
    let mut extra_game_args = launch_cfg.game_args.clone();
    extra_game_args.extend(game_args.iter().cloned());
    let mut extra_jvm_args = launch_cfg.jvm_args.clone();
    extra_jvm_args.extend(jvm_args.iter().cloned());

    let options = LaunchOptions {
        width,
        height,
        extra_game_args,
        jvm_args: extra_jvm_args,
        memory: heap,
        initial_memory: initial_heap,
        proxy: cfg.general.proxy.clone(),
        brand: launch_cfg.brand.clone(),
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

    // A pre-launch command is the user's own: run it in the game directory, show its
    // output, and stop if it fails rather than starting the game in a state the
    // command was meant to prepare.
    if !launch_cfg.pre_launch.trim().is_empty() {
        run_pre_launch(&launch_cfg.pre_launch, &game_dir, out)?;
    }

    out.human(format!("launching {id} as {}", identity.name));
    let code = launch::spawn::spawn(&plan)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Runs `launch.pre_launch` before the game, in the game directory.
///
/// It is the user's command, so its output and its exit code are theirs too: output is
/// inherited rather than captured, and a non-zero status stops the launch with a code
/// that says which piece failed.
fn run_pre_launch(command: &str, game_dir: &std::path::Path, out: &Out) -> MlrcResult<()> {
    out.human(format!("pre-launch: {command}"));

    #[cfg(windows)]
    let child = std::process::Command::new("cmd")
        .arg("/C")
        .arg(command)
        .current_dir(game_dir)
        .spawn();

    #[cfg(not(windows))]
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(game_dir)
        .spawn();

    let status = child
        .map_err(|err| {
            MlrcError::new(
                ErrorCode::PreLaunch,
                format!("cannot run the pre-launch command: {err}"),
            )
            .with("command", command.to_string())
        })?
        .wait()
        .map_err(|err| {
            MlrcError::new(
                ErrorCode::PreLaunch,
                format!("the pre-launch command could not be waited for: {err}"),
            )
            .with("command", command.to_string())
        })?;

    if !status.success() {
        return Err(MlrcError::new(
            ErrorCode::PreLaunch,
            format!(
                "the pre-launch command failed with {}",
                status.code().unwrap_or(-1)
            ),
        )
        .with("command", command.to_string())
        .with("hint", "fix or clear launch.pre_launch, then launch again"));
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
    let state = InstalledState::load(&root, version)?;
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
        let dir = root.state_dir(&id);
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
            jvm_args,
            game_args,
            pre_launch,
            brand,
        } => {
            // A fork holds only a document and a hardlinked jar; the version it points
            // at has to exist already.
            match InstalledState::load(&root, version)? {
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

            let mut fork = config::ForkConfig {
                launch: LaunchConfig {
                    brand: brand.clone().unwrap_or_default(),
                    jvm_args: jvm_args.to_vec(),
                    game_args: game_args.to_vec(),
                    pre_launch: pre_launch.clone().unwrap_or_default(),
                    game_language: String::new(),
                },
                ..config::ForkConfig::default()
            };
            // `--memory` writes the same key `config set java.xmx --instance` writes.
            if let Some(memory) = memory {
                fork.java.set_key("java.xmx", memory)?;
            }
            let instance = Instance::create(&root, name, version, fork)?;
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
        InstanceCommand::Import {
            file,
            name,
            java,
            dry_run,
        } => {
            if *dry_run {
                let preview = mods::modpack::preview(file)?;
                let kind = match preview.kind {
                    mods::modpack::Kind::Modrinth => "modrinth",
                    mods::modpack::Kind::CurseForge => "curseforge",
                };

                if out.is_json() {
                    let files: Vec<_> = preview
                        .files
                        .iter()
                        .map(|entry| json!({"path": entry.label, "bytes": entry.bytes}))
                        .collect();
                    out.json_event(json!({
                        "schema": 1,
                        "event": "pack_preview",
                        "kind": kind,
                        "name": preview.name,
                        "minecraft": preview.minecraft,
                        "files": files,
                        "skipped": preview.skipped,
                        "overrides": preview.overrides,
                    }));
                    return Ok(());
                }

                detail(out, "pack", &format!("{} ({kind})", preview.name));
                detail(out, "minecraft", &preview.minecraft);
                if let Some((loader, version)) = &preview.loader {
                    detail(out, "loader", &format!("{loader} {version}"));
                }
                detail(out, "overrides", &preview.overrides.to_string());
                if preview.skipped > 0 {
                    detail(out, "skipped", &format!("{} optional", preview.skipped));
                }
                out.human(format!("{} file(s):", preview.files.len()));
                for entry in &preview.files {
                    if entry.bytes > 0 {
                        out.human(format!("  {} ({})", entry.label, human_bytes(entry.bytes)));
                    } else {
                        out.human(format!("  {}", entry.label));
                    }
                }
                return Ok(());
            }

            let cfg = Config::load(&root.config_path())?;
            let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
            let client = HttpClient::with_proxy(mirror, &cfg.general.proxy)?;
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
                proxy: cfg.general.proxy.clone(),
            };

            let sink = Sink::for_run(cli.json, cli.quiet);
            let summary = mods::modpack::import(
                &client,
                &root,
                file,
                name.as_deref(),
                cfg.curseforge_key().as_deref(),
                &options,
                &sink,
            )
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

async fn cmd_pack(cli: &Cli, out: &Out, command: &PackCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;
    let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
    let client = HttpClient::with_proxy(mirror, &cfg.general.proxy)?;

    match command {
        PackCommand::Search {
            query,
            kind,
            instance,
            version,
            mc,
            limit,
        } => {
            let kind = pack_kind(kind)?;
            let instance = load_instance(&root, instance.as_deref())?;
            // A pack has no loader, so `--mc` alone is enough; otherwise the game
            // version comes from the version or fork being targeted.
            let game = match mc {
                Some(value) => value.clone(),
                None => {
                    let (id, _) = pack_target(&root, &cfg, version.as_deref(), instance.as_ref())?;
                    mods::minecraft_of(&root, &id)?
                }
            };

            let hits = mods::modrinth::search(&client, query, &game, None, kind, *limit).await?;

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
                out.json_event(json!({"schema": 1, "event": "pack_search", "hits": items}));
                return Ok(());
            }

            if hits.is_empty() {
                out.human(format!(
                    "no {} match {query} for minecraft {game}",
                    kind.as_str()
                ));
                return Ok(());
            }

            for hit in hits {
                out.human(format!(
                    "{:<40}{:>10}  {}",
                    truncate(&hit.slug, 40),
                    thousands(hit.downloads),
                    truncate(&hit.title, 60)
                ));
            }
            Ok(())
        }
        PackCommand::Install {
            projects,
            kind,
            world,
            instance,
            version,
            dry_run,
        } => {
            if projects.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no project given; run `mlrc pack search` to find one",
                ));
            }

            let flag_kind = match kind {
                Some(text) => Some(pack_kind(text)?),
                None => None,
            };
            let instance = load_instance(&root, instance.as_deref())?;
            let (id, game_dir) = pack_target(&root, &cfg, version.as_deref(), instance.as_ref())?;
            let platform = mods::Platform {
                mc: mods::minecraft_of(&root, &id)?,
                loader: String::new(),
            };

            let sink = Sink::for_run(cli.json, cli.quiet);
            for project in projects {
                // Modrinth says what the project is; the flag only overrides it, so
                // `pack install sodium` cannot quietly land in `resourcepacks/`.
                let kind = match flag_kind {
                    Some(kind) => kind,
                    None => {
                        let reported = mods::modrinth::project_type(&client, project).await?;
                        match mods::Kind::parse(&reported) {
                            Some(mods::Kind::Mod) => {
                                return Err(MlrcError::new(
                                    ErrorCode::Usage,
                                    format!("{project} is a mod; use `mlrc mod install {project}`"),
                                ))
                            }
                            Some(kind) => kind,
                            None => {
                                return Err(MlrcError::new(
                                    ErrorCode::Usage,
                                    format!(
                                        "{project} is a {reported}, which this launcher does not install"
                                    ),
                                ))
                            }
                        }
                    }
                };

                if *dry_run {
                    let plan = mods::plan(
                        &client,
                        &platform,
                        project,
                        kind,
                        mods::Source::Modrinth,
                        None,
                    )
                    .await?;
                    for item in plan {
                        out.human(format!(
                            "would install {} ({}) into {}",
                            item.filename,
                            item.version,
                            kind.dir(&game_dir, world.as_deref()).display()
                        ));
                    }
                    continue;
                }

                let installed = mods::install_from(
                    &client,
                    &game_dir,
                    &platform,
                    project,
                    kind,
                    world.as_deref(),
                    mods::Source::Modrinth,
                    None,
                    &sink,
                )
                .await?;
                sink.finish();

                for item in installed {
                    let action = if item.already_present {
                        "already present"
                    } else {
                        "installed"
                    };
                    out.human(format!("{action} {} ({})", item.filename, kind.as_str()));
                }
            }

            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "packs_installed",
                    "directory": game_dir.display().to_string(),
                }));
            }
            Ok(())
        }
        PackCommand::List {
            kind,
            instance,
            version,
        } => {
            let only = match kind {
                Some(text) => Some(pack_kind(text)?),
                None => None,
            };
            let instance = load_instance(&root, instance.as_deref())?;
            let (_, game_dir) = pack_target(&root, &cfg, version.as_deref(), instance.as_ref())?;
            let manifest = mods::manifest::Manifest::load(&game_dir)?;

            let kinds: Vec<mods::Kind> = match only {
                Some(kind) => vec![kind],
                None => vec![
                    mods::Kind::ResourcePack,
                    mods::Kind::Shader,
                    mods::Kind::DataPack,
                ],
            };

            let mut rows = Vec::new();
            for kind in kinds {
                let dir = kind.dir(&game_dir, None);
                if !dir.is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    if !entry.file_type()?.is_file() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    let tracked = manifest
                        .mods
                        .iter()
                        .find(|item| item.filename == name)
                        .map(|item| item.project.clone())
                        .unwrap_or_default();
                    rows.push((kind, name, tracked));
                }
            }
            rows.sort_by(|left, right| {
                (left.0.folder(), &left.1).cmp(&(right.0.folder(), &right.1))
            });

            if out.is_json() {
                let items: Vec<_> = rows
                    .iter()
                    .map(|(kind, name, project)| {
                        json!({"kind": kind.as_str(), "filename": name, "project": project})
                    })
                    .collect();
                out.json_event(json!({"schema": 1, "event": "packs", "packs": items}));
                return Ok(());
            }

            if rows.is_empty() {
                out.human("no packs installed");
                return Ok(());
            }
            out.human(format!("{:<14}{:<46}{}", "KIND", "FILE", "PROJECT"));
            for (kind, name, project) in rows {
                out.human(format!(
                    "{:<14}{:<46}{}",
                    kind.as_str(),
                    truncate(&name, 46),
                    project
                ));
            }
            Ok(())
        }
        PackCommand::Remove {
            files,
            kind,
            instance,
            version,
        } => {
            if files.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no file given; run `mlrc pack list` to see them",
                ));
            }
            let only = match kind {
                Some(text) => Some(pack_kind(text)?),
                None => None,
            };
            let instance = load_instance(&root, instance.as_deref())?;
            let (_, game_dir) = pack_target(&root, &cfg, version.as_deref(), instance.as_ref())?;

            let kinds: Vec<mods::Kind> = match only {
                Some(kind) => vec![kind],
                None => vec![
                    mods::Kind::ResourcePack,
                    mods::Kind::Shader,
                    mods::Kind::DataPack,
                ],
            };

            let mut removed = Vec::new();
            for kind in kinds {
                let dir = kind.dir(&game_dir, None);
                for file in files {
                    let target = dir.join(file);
                    if target.is_file() {
                        std::fs::remove_file(&target)?;
                        removed.push((kind, file.clone()));
                    }
                }
            }

            if removed.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::ModNotFound,
                    format!("no such pack: {}", files.join(", ")),
                ));
            }

            for (kind, file) in &removed {
                out.human(format!("removed {} ({})", file, kind.as_str()));
            }
            if out.is_json() {
                out.json_event(json!({
                    "schema": 1,
                    "event": "packs_removed",
                    "files": removed.iter().map(|(_, file)| file.clone()).collect::<Vec<_>>(),
                }));
            }
            Ok(())
        }
    }
}

/// The version a pack command acts on: explicit, from a fork, or the default.
///
/// Packs do not need a loader, so this returns the game directory and the id rather
/// than a `Platform`: `mlrc pack install` has to work on a vanilla version.
fn pack_target(
    root: &DataRoot,
    cfg: &Config,
    version: Option<&str>,
    instance: Option<&Instance>,
) -> MlrcResult<(String, std::path::PathBuf)> {
    let id =
        match (version, instance) {
            (Some(id), _) => id.to_string(),
            (None, Some(instance)) => instance.name.clone(),
            (None, None) if !cfg.general.default_version.is_empty() => {
                cfg.general.default_version.clone()
            }
            (None, None) => return Err(MlrcError::new(
                ErrorCode::Usage,
                "no version to install into; pass --version or --instance, or `mlrc use <version>`",
            )),
        };
    let game_dir = root.game_dir(&id);
    Ok((id, game_dir))
}

/// Opens a url in the user's browser, best effort.
///
/// The url is printed either way, so a machine without a desktop - a server, a
/// container - still gets something to copy.
/// What the unified pass client component looks like, for `auth status`.
fn nide8_client_state(root: &DataRoot, cfg: &Config) -> String {
    let path = nide8_jar_path(root, cfg);
    match std::fs::metadata(&path) {
        Ok(meta) => format!("{} ({} bytes)", path.display(), meta.len()),
        Err(_) => format!(
            "missing at {}; run `mlrc auth login` to download it",
            path.display()
        ),
    }
}
fn open_in_browser(url: &str) -> MlrcResult<()> {
    #[cfg(windows)]
    let spawned = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();

    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(url).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(url).spawn();

    spawned
        .map(|_| ())
        .map_err(|err| MlrcError::new(ErrorCode::Io, err.to_string()))
}
/// Makes sure `nide8auth.jar` is on disk, offering to download it when it is not.
///
/// The service publishes the jar and the server document carries its hash, so a download
/// can be verified before it is used. When the file is already there it is checked
/// instead, and a mismatch is a warning rather than an error: a server may have just
/// replaced its component, and refusing to launch would be worse than saying so.
///
/// A terminal is asked before anything is downloaded. A script or `--json` run cannot
/// answer, and failing there would break the launch over a question whose answer is
/// always yes, so it downloads and says so.
async fn ensure_nide8_jar(
    cli: &Cli,
    out: &Out,
    root: &DataRoot,
    cfg: &Config,
    client: &HttpClient,
    server: Option<&auth::nide8::Server>,
) -> MlrcResult<std::path::PathBuf> {
    let target = nide8_jar_path(root, cfg);
    let expected = server.map(|server| server.jar_sha1.as_str()).unwrap_or("");

    if target.is_file() {
        if let (false, Ok(bytes)) = (expected.is_empty(), std::fs::read(&target)) {
            let actual = mods::sha1_of(&bytes);
            if actual != expected {
                out.human("warning: nide8auth.jar does not match the server's jarHash");
                out.human(format!("  local:  {actual}"));
                out.human(format!("  server: {expected}"));
                out.human("  `mlrc auth login` downloads a fresh copy");
            }
        }
        return Ok(target);
    }

    let consented = if interactive(cli, out) {
        eprint!("nide8auth.jar is missing. download it from login.mc-user.com now? [Y/n] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|err| MlrcError::new(ErrorCode::Io, format!("cannot read stdin: {err}")))?;
        matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        )
    } else {
        out.human("nide8auth.jar is missing; downloading it");
        true
    };

    if !consented {
        return Err(MlrcError::new(
            ErrorCode::AuthFailed,
            "unified pass needs nide8auth.jar and it was not downloaded",
        )
        .with(
            "hint",
            "re-run and accept the download, or set `mlrc config set auth.nide8_jar <path>`",
        ));
    }

    let path = auth::nide8::download_jar(client, &target, expected).await?;
    out.human(format!("nide8auth.jar -> {}", path.display()));
    Ok(path)
}

/// The unified pass client jar: configured, or the conventional spot in the data folder.
fn nide8_jar_path(root: &DataRoot, cfg: &Config) -> std::path::PathBuf {
    if cfg.auth.nide8_jar.trim().is_empty() {
        root.injector_dir().join("nide8auth.jar")
    } else {
        std::path::PathBuf::from(cfg.auth.nide8_jar.trim())
    }
}

fn nide8_jar(root: &DataRoot, cfg: &Config) -> MlrcResult<std::path::PathBuf> {
    let path = nide8_jar_path(root, cfg);

    if path.is_file() {
        Ok(path)
    } else {
        Err(MlrcError::new(
            ErrorCode::AuthFailed,
            format!("nide8auth.jar is missing at {}", path.display()),
        )
        .with(
            "hint",
            "put the server's nide8auth.jar there, or set `mlrc config set auth.nide8_jar <path>`",
        ))
    }
}
fn pack_kind(text: &str) -> MlrcResult<mods::Kind> {
    match mods::Kind::parse(text) {
        Some(mods::Kind::Mod) => Err(MlrcError::new(
            ErrorCode::Usage,
            "mods are managed by `mlrc mod`; `mlrc pack` handles resource packs, shaders and data packs",
        )),
        Some(kind) => Ok(kind),
        None => Err(MlrcError::new(
            ErrorCode::Usage,
            format!("unknown kind {text} (expected resourcepack, shader or datapack)"),
        )),
    }
}
async fn cmd_mod(cli: &Cli, out: &Out, command: &ModCommand) -> MlrcResult<()> {
    let root = DataRoot::resolve(cli.data_dir.as_deref())?;
    root.ensure()?;
    let cfg = Config::load(&root.config_path())?;
    let mirror = Mirror::from_config(cfg.general.mirror).unwrap_or(Mirror::Official);
    let client = HttpClient::with_proxy(mirror, &cfg.general.proxy)?;

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
            let (platform, _) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                mc.as_deref(),
                loader.as_deref(),
            )?;
            let hits = mods::modrinth::search(
                &client,
                query,
                &platform.mc,
                Some(&platform.loader),
                mods::Kind::Mod,
                *limit,
            )
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
            dry_run,
        } => {
            if projects.is_empty() {
                return Err(MlrcError::new(
                    ErrorCode::Usage,
                    "no project given; run `mlrc mod install sodium`",
                ));
            }

            let source = mods::Source::parse(source)?;
            let key = cfg.curseforge_key();
            let instance = load_instance(&root, instance.as_deref())?;
            // A mod needs a loader; packs do not, which is why this path uses
            // `mod_platform` and `pack_target` exists for the other three kinds.
            let (platform, target) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = mods_game_dir(&root, instance.as_ref(), target)?;

            if *dry_run {
                let mut planned = Vec::new();
                for project in projects {
                    planned.extend(
                        mods::plan(
                            &client,
                            &platform,
                            project,
                            mods::Kind::Mod,
                            source,
                            key.as_deref(),
                        )
                        .await?,
                    );
                }

                if out.is_json() {
                    let items: Vec<_> = planned
                        .iter()
                        .map(|module| {
                            json!({
                                "name": module.name,
                                "version": module.version,
                                "filename": module.filename,
                                "bytes": module.size,
                            })
                        })
                        .collect();
                    out.json_event(json!({"schema": 1, "event": "mods_planned", "mods": items}));
                    return Ok(());
                }

                if planned.is_empty() {
                    out.human("nothing to install");
                    return Ok(());
                }

                out.human(format!(" {:<30}{:<28}{}", "PROJECT", "VERSION", "FILE"));
                let mut total = 0u64;
                for module in &planned {
                    total += module.size;
                    out.human(format!(
                        " {:<30}{:<28}{}",
                        truncate(&module.name, 29),
                        truncate(&module.version, 27),
                        module.filename
                    ));
                }
                out.human(format!(
                    "{} file(s) for {}, {}",
                    planned.len(),
                    mods::mods_dir(&game_dir).display(),
                    if total > 0 {
                        human_bytes(total)
                    } else {
                        "size unknown".to_string()
                    }
                ));
                return Ok(());
            }

            let sink = Sink::for_run(cli.json, cli.quiet);
            let mut total = 0u64;
            let mut names = Vec::new();

            for project in projects {
                let installed = mods::install_from(
                    &client,
                    &game_dir,
                    &platform,
                    project,
                    mods::Kind::Mod,
                    None,
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
            let (_, target) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = mods_game_dir(&root, instance.as_ref(), target)?;
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
            let (_, target) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = mods_game_dir(&root, instance.as_ref(), target)?;

            let sink = Sink::for_run(cli.json, cli.quiet);
            let report = mods::update(
                &client,
                &game_dir,
                *check,
                cfg.curseforge_key().as_deref(),
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
            let (_, target) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = mods_game_dir(&root, instance.as_ref(), target)?;

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
            let (_, target) = mod_platform(
                &root,
                &cfg,
                version.as_deref(),
                instance.as_ref(),
                None,
                None,
            )?;
            let game_dir = mods_game_dir(&root, instance.as_ref(), target)?;

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

fn detail(out: &Out, label: &str, value: &str) {
    out.human(format!("{:<13}{value}", format!("{label}:")));
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
) -> MlrcResult<(mods::Platform, Option<String>)> {
    let explicit = mc.is_some() && loader.is_some();

    let mut target = None;
    let mut platform = match (mc, loader) {
        (Some(mc), Some(loader)) => mods::Platform {
            mc: mc.to_string(),
            loader: loader.to_string(),
        },
        _ => {
            let id = match version {
                Some(id) => id.to_string(),
                None => match instance {
                    Some(instance) => instance.name.clone(),
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
            target = Some(id.clone());
            mods::platform(root, &id)?
        }
    };

    if let Some(mc) = mc {
        platform.mc = mc.to_string();
    }
    if let Some(loader) = loader {
        platform.loader = loader.to_string();
    }

    if !["fabric", "quilt", "forge", "neoforge"].contains(&platform.loader.as_str()) {
        return Err(MlrcError::new(
            ErrorCode::Usage,
            format!(
                "unknown loader {} (expected fabric, quilt, forge or neoforge)",
                platform.loader
            ),
        ));
    }

    // With both halves named explicitly there is no version to put files in.
    Ok((platform, if explicit { None } else { target }))
}

/// The game directory a mod command should touch.
fn mods_game_dir(
    root: &DataRoot,
    instance: Option<&Instance>,
    target: Option<String>,
) -> MlrcResult<std::path::PathBuf> {
    match (instance, target) {
        (Some(instance), _) => Ok(Instance::game_dir(root, &instance.name)),
        (None, Some(id)) => Ok(root.game_dir(&id)),
        (None, None) => Err(MlrcError::new(
            ErrorCode::Usage,
            "no version to install into; pass --version or --instance",
        )),
    }
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
                "host java: {} at {} (java.custom=true)",
                host.major,
                host.executable.display()
            )),
            None => out.human("host java: java.custom=true but no usable JDK was found"),
        }
    } else {
        out.human("host java: ignored (java.custom=false)");
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

    let sink = Sink::for_run(cli.json, cli.quiet);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_answers_mean_what_they_say() {
        assert_eq!(parse_count("", 921).expect("enter"), 10);
        assert_eq!(parse_count("  ", 921).expect("blank"), 10);
        assert_eq!(parse_count("all", 921).expect("all"), 921);
        assert_eq!(parse_count("*", 921).expect("star"), 921);
        assert_eq!(parse_count("0", 921).expect("zero"), 921);
        assert_eq!(parse_count("25", 921).expect("number"), 25);
        // More than there are is not an error, it is everything.
        assert_eq!(parse_count("5000", 921).expect("too many"), 921);
        // A short list offers what it has, not ten.
        assert_eq!(parse_count("", 3).expect("short list"), 3);
    }

    #[test]
    fn a_bad_answer_is_a_usage_error() {
        let error = parse_count("twenty", 921).expect_err("refused");
        assert_eq!(error.code, ErrorCode::Usage);
        assert!(error.message.contains("twenty"));
    }
}
