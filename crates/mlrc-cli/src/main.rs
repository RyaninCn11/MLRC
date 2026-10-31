mod cli;
mod commands;
mod json_sink;
mod progress;
mod render;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::Cli;
use render::Out;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    let out = Out::new(cli.json, cli.quiet);

    match commands::run(&cli, &out).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            render::error(&out, &err);
            ExitCode::from(err.code.exit_code().clamp(0, 255) as u8)
        }
    }
}

fn init_tracing(verbose: u8, quiet: bool) {
    let level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            _ => "debug",
        }
    };

    let filter = EnvFilter::try_from_env("MLRC_LOG")
        .unwrap_or_else(|_| EnvFilter::new(format!("mlrc_core={level},mlrc_cli={level}")));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
