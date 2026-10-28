//! Command surface. Everything here is presentation: the engine lives in mlrc-core.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "mlrc",
    version,
    about = "Minecraft Launcher of Rust Command-line",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Data directory. Defaults to <directory of the executable>/data (portable).
    #[arg(long, global = true, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,

    /// Emit NDJSON events on stdout instead of human text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential human output.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Increase log verbosity (repeatable).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Download source. `auto` probes both and remembers the faster one.
    #[arg(long, global = true, value_enum)]
    pub mirror: Option<MirrorArg>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MirrorArg {
    /// Probe both and keep the faster, remembering the choice
    Auto,
    /// Mojang, piston, libraries.minecraft.net
    Official,
    /// bmclapi2.bangbang93.com
    Bmclapi,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install one Minecraft version and, if needed, the Java runtime it requires
    Install {
        /// Version id exactly as the manifest spells it, e.g. 1.18.2
        version: String,

        /// What to do when the required Java runtime is missing
        #[arg(long, value_enum, default_value = "auto")]
        java: JavaMode,

        /// Accept every prompt
        #[arg(long)]
        yes: bool,

        /// Print the plan without downloading anything
        #[arg(long)]
        dry_run: bool,

        /// Parallel downloads
        #[arg(long, default_value_t = 8)]
        threads: usize,

        /// Install a loader on top of the version: `fabric` or `fabric:<loader version>`
        #[arg(long, value_name = "NAME[:VERSION]")]
        loader: Option<String>,
    },

    /// Launch an installed version
    Launch {
        /// Version to launch; defaults to the one set with `mlrc use`
        version: Option<String>,

        /// Skip Microsoft authentication and use an offline identity
        #[arg(long)]
        offline: bool,

        /// Player name for offline mode
        #[arg(long)]
        username: Option<String>,

        #[arg(long)]
        width: Option<u32>,

        #[arg(long)]
        height: Option<u32>,

        /// Print the command line without starting the game
        #[arg(long)]
        dry_run: bool,

        /// Use the game directory and version of a named instance
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,

        /// Language for the game, e.g. zh_cn. Written to options.txt before launch.
        #[arg(long, value_name = "CODE")]
        lang: Option<String>,

        /// Heap size for the game, e.g. 4G or 4096M. Overrides the config.
        #[arg(long, value_name = "SIZE")]
        memory: Option<String>,

        /// Extra JVM flag, repeatable, e.g. --jvm-arg -XX:+UseG1GC
        #[arg(long, value_name = "ARG", allow_hyphen_values = true)]
        jvm_arg: Vec<String>,

        /// Extra arguments for the game, after `--`
        #[arg(last = true)]
        game_args: Vec<String>,
    },

    /// Manage named game directories
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },

    /// Search and install mods from Modrinth
    Mod {
        #[command(subcommand)]
        command: ModCommand,
    },

    /// List installed versions
    List {
        /// List every version known to the manifest (requires network)
        #[arg(long)]
        all: bool,
    },

    /// Set the default version used by a bare `mlrc launch`
    Use { version: String },

    /// Remove an installed version
    Uninstall { version: String },

    /// Manage MLRC-managed Java runtimes
    Java {
        #[command(subcommand)]
        command: JavaCommand,
    },

    /// Manage Microsoft accounts
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Read or write configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Manage the content-addressed cache
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },

    /// Check the environment and report problems
    Doctor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum JavaMode {
    /// Prompt when interactive, fail with exit code 5 when not
    Auto,
    /// Install the runtime without asking
    Install,
    /// Continue without the runtime; launching will fail later
    Skip,
}

#[derive(Debug, Subcommand)]
pub enum InstanceCommand {
    /// Create an instance for an installed version
    Create {
        name: String,
        #[arg(long)]
        version: String,
        /// Heap for this instance, e.g. 4G. Defaults to the global setting.
        #[arg(long, value_name = "SIZE")]
        memory: Option<String>,
    },
    /// List instances
    List,
    /// Remove an instance and its game directory
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
pub enum ModCommand {
    /// Search Modrinth; the loader and Minecraft version come from an installed version
    Search {
        query: String,
        /// Minecraft version, overriding the one taken from the version
        #[arg(long)]
        mc: Option<String>,
        /// Mod loader: fabric, forge or neoforge
        #[arg(long)]
        loader: Option<String>,
        /// Take the loader and version from an instance
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
        /// Take the loader and version from an installed version
        #[arg(long)]
        version: Option<String>,
        /// How many results to show
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Install mods, together with their required dependencies
    Install {
        /// Modrinth slug or project id
        projects: Vec<String>,
        /// Target an instance's game directory
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
        /// Target another installed version
        #[arg(long)]
        version: Option<String>,
    },
    /// List the jars in a mods directory
    List {
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
        #[arg(long)]
        version: Option<String>,
    },
    /// Replace installed mods with newer versions from Modrinth
    Update {
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Report what would change without downloading anything
        #[arg(long)]
        check: bool,
    },
    /// Remove mods by slug, project id or file name
    Remove {
        mods: Vec<String>,
        #[arg(long, value_name = "NAME")]
        instance: Option<String>,
        #[arg(long)]
        version: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum JavaCommand {
    /// List managed runtimes
    List,
    /// Install a runtime by Mojang component name or by major version
    Install {
        target: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove a managed runtime
    Remove { major: u32 },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in with a Microsoft account
    Login,
    /// Remove stored credentials
    Logout,
    /// Show the current account
    Status,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print one key
    Get { key: String },
    /// Set one key
    Set { key: String, value: String },
    /// Print every known key
    List,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Drop objects that no installed version references
    Prune {
        #[arg(long)]
        keep_days: Option<u32>,
    },
}
