//! Turning an installed version plus an identity into a concrete command line.

pub mod memory;
pub mod natives;
pub mod options;
pub mod spawn;
pub mod template;

use std::path::{Path, PathBuf};

use crate::auth::{uuid_without_dashes, Identity};
use crate::error::MlrcResult;
use crate::java::discover::JavaInstallation;
use crate::meta::rule::Env;
use crate::meta::version::VersionJson;
use crate::plan::install::InstallPlan;
use crate::port::DataRoot;

use template::Variables;

#[cfg(windows)]
const PATH_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const PATH_SEPARATOR: &str = ":";

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub extra_game_args: Vec<String>,
    /// Extra JVM flags from the user; when set, `default-user-jvm` is not applied.
    pub jvm_args: Vec<String>,
    /// `-Xmx` value, already normalised by [`memory::normalize`].
    pub memory: Option<String>,
    /// `-Xms` value, already normalised; `None` leaves Java's default alone.
    pub initial_memory: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub java: PathBuf,
    pub args: Vec<String>,
    pub main_class: String,
    pub game_args: Vec<String>,
    pub cwd: PathBuf,
}

impl LaunchPlan {
    /// Full argv, for `--dry-run` and for logs.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + self.game_args.len() + 2);
        argv.push(self.java.display().to_string());
        argv.extend(self.args.iter().cloned());
        argv.push(self.main_class.clone());
        argv.extend(self.game_args.iter().cloned());
        argv
    }
}

pub fn build(
    version: &VersionJson,
    plan: &InstallPlan,
    root: &DataRoot,
    identity: &Identity,
    java: &JavaInstallation,
    game_dir: &Path,
    options: &LaunchOptions,
) -> MlrcResult<LaunchPlan> {
    let version_dir = root.version_dir(&version.id);
    let natives_dir = version_dir.join("natives");
    let game_dir = game_dir.to_path_buf();
    std::fs::create_dir_all(&game_dir)?;

    let mut features: Vec<&str> = Vec::new();
    let custom_resolution = options.width.is_some() && options.height.is_some();
    if custom_resolution {
        features.push("has_custom_resolution");
    }
    let env = Env::detect().with_features(&features);

    let classpath = classpath(version, plan, &version_dir);

    let mut vars = Variables::new();
    vars.set("auth_player_name", identity.name.clone());
    vars.set("auth_uuid", uuid_without_dashes(&identity.uuid));
    vars.set("auth_access_token", identity.access_token.clone());
    vars.set("auth_session", identity.access_token.clone());
    vars.set("auth_xuid", identity.xuid.clone());
    vars.set("clientid", identity.client_id.clone());
    vars.set("user_type", identity.user_type.clone());
    vars.set("user_properties", "{}");
    vars.set("version_name", version.id.clone());
    vars.set("version_type", version.kind.clone());
    vars.set("game_directory", game_dir.display().to_string());
    vars.set("assets_root", root.assets_dir().display().to_string());
    vars.set(
        "assets_index_name",
        version
            .asset_index
            .as_ref()
            .map(|index| index.id.clone())
            .or_else(|| version.assets.clone())
            .unwrap_or_else(|| "legacy".to_string()),
    );
    vars.set("natives_directory", natives_dir.display().to_string());
    vars.set("classpath", classpath.clone());
    // Used by Forge/NeoForge to build their module path; without them the literal
    // `${library_directory}` reaches the JVM and the bootstrap cannot find its modules.
    vars.set(
        "library_directory",
        version_dir.join("libraries").display().to_string(),
    );
    vars.set("classpath_separator", PATH_SEPARATOR);
    vars.set("launcher_name", "mlrc");
    vars.set("launcher_version", env!("CARGO_PKG_VERSION"));
    if let (Some(width), Some(height)) = (options.width, options.height) {
        vars.set("resolution_width", width.to_string());
        vars.set("resolution_height", height.to_string());
    }

    let mut jvm: Vec<String> = Vec::new();

    // Only when the user brought no flags of their own: the field exists exactly to
    // stop a launcher from launching 26.x with settings from 2011.
    if options.jvm_args.is_empty() {
        if let Some(arguments) = &version.arguments {
            let mut collected = Vec::new();
            for argument in &arguments.default_user_jvm {
                argument.collect(&env, &mut collected);
            }
            for argument in collected {
                jvm.push(vars.expand(&argument));
            }
        }
    }

    let mut version_jvm: Vec<String> = Vec::new();
    if let Some(arguments) = &version.arguments {
        for argument in &arguments.jvm {
            argument.collect(&env, &mut version_jvm);
        }
    }
    if version_jvm.is_empty() {
        // Pre-1.13 documents have no jvm arguments at all; build the classic pair.
        version_jvm.push(format!("-Djava.library.path={}", natives_dir.display()));
        version_jvm.push("-cp".to_string());
        version_jvm.push(classpath);
    }
    for argument in version_jvm {
        jvm.push(vars.expand(&argument));
    }

    if let Some(client) = version
        .logging
        .as_ref()
        .and_then(|logging| logging.client.as_ref())
    {
        if !client.argument.is_empty() {
            let file = version_dir.join("logging").join(client.file.file_name());
            let argument = client
                .argument
                .replace("${path}", &file.display().to_string());
            jvm.push(argument);
        }
    }

    jvm.extend(options.jvm_args.iter().cloned());

    // Some loaders write remapped jars and image caches through `java.io.tmpdir`.
    // Redirect it into the data directory: portable, and immune to a locked-down
    // system temp directory.
    let tmp_dir = root.tmp_dir();
    std::fs::create_dir_all(&tmp_dir)?;
    if !jvm
        .iter()
        .any(|argument| argument.starts_with("-Djava.io.tmpdir"))
    {
        jvm.insert(0, format!("-Djava.io.tmpdir={}", tmp_dir.display()));
    }

    // The game talks to Mojang as well; give it the same proxy the launcher uses,
    // unless the instance already pins its own.
    if !jvm
        .iter()
        .any(|argument| argument.starts_with("-Dhttp.proxyHost"))
    {
        let mut with_proxy = crate::fetch::proxy::java_args();
        with_proxy.append(&mut jvm);
        jvm = with_proxy;
    }

    // A requested heap wins over whatever the document or `default-user-jvm` carries,
    // otherwise a modded pack keeps the 2G default and dies with an OOM.
    if let Some(xms) = &options.initial_memory {
        jvm.retain(|argument| !argument.starts_with("-Xms"));
        jvm.insert(0, format!("-Xms{xms}"));
    }
    match &options.memory {
        Some(memory) => {
            jvm.retain(|argument| !argument.starts_with("-Xmx"));
            jvm.insert(0, format!("-Xmx{memory}"));
        }
        None if !jvm.iter().any(|argument| argument.starts_with("-Xmx")) => {
            jvm.insert(0, "-Xmx2G".to_string());
        }
        None => {}
    }

    let game_args = game_arguments(version, &env, &vars, options);

    Ok(LaunchPlan {
        java: java.executable.clone(),
        args: jvm,
        main_class: version.main_class.clone(),
        game_args,
        cwd: game_dir,
    })
}

fn game_arguments(
    version: &VersionJson,
    env: &Env,
    vars: &Variables,
    options: &LaunchOptions,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();

    if let Some(arguments) = &version.arguments {
        for argument in &arguments.game {
            argument.collect(env, &mut collected);
        }
    }

    // 1.12 and older: one flat string.
    if collected.is_empty() {
        if let Some(legacy) = &version.minecraft_arguments {
            collected = legacy.split_whitespace().map(str::to_string).collect();
        }
    }

    let mut args: Vec<String> = collected.iter().map(|arg| vars.expand(arg)).collect();
    args.extend(options.extra_game_args.iter().cloned());
    args
}

fn classpath(version: &VersionJson, plan: &InstallPlan, version_dir: &Path) -> String {
    let mut entries: Vec<String> = Vec::with_capacity(plan.libraries.len() + 1);
    entries.push(
        version_dir
            .join(format!("{}.jar", version.id))
            .display()
            .to_string(),
    );
    for library in &plan.libraries {
        entries.push(
            version_dir
                .join("libraries")
                .join(&library.path)
                .display()
                .to_string(),
        );
    }
    entries.join(PATH_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::version::VersionJson;

    fn version(json: &str) -> VersionJson {
        serde_json::from_str(json).expect("version parses")
    }

    #[test]
    fn legacy_arguments_are_split_and_expanded() {
        let document = version(
            r#"{"id":"1.12.2","mainClass":"net.minecraft.client.main.Main",
                "minecraftArguments":"--username ${auth_player_name} --version ${version_name}"}"#,
        );
        let mut vars = Variables::new();
        vars.set("auth_player_name", "Steve");
        vars.set("version_name", "1.12.2");

        let args = game_arguments(&document, &Env::detect(), &vars, &LaunchOptions::default());
        assert_eq!(args, vec!["--username", "Steve", "--version", "1.12.2"]);
    }

    #[test]
    fn extra_arguments_are_appended_last() {
        let document =
            version(r#"{"id":"1.12.2","arguments":{"game":["--username","${auth_player_name}"]}}"#);
        let mut vars = Variables::new();
        vars.set("auth_player_name", "Steve");

        let options = LaunchOptions {
            extra_game_args: vec!["--server".to_string(), "localhost".to_string()],
            ..Default::default()
        };
        let args = game_arguments(&document, &Env::detect(), &vars, &options);
        assert_eq!(args, vec!["--username", "Steve", "--server", "localhost"]);
    }
}
