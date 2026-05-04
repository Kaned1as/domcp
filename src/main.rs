mod cli;
mod container;
mod dockerfile;
mod proxy;
mod runner;
mod signal;

use anyhow::Result;
use log::info;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let args = cli::parse();

    info!(
        "domcp v{} — Dockerizing MCP server for safer interaction",
        env!("CARGO_PKG_VERSION")
    );

    runner::run(args)
}
