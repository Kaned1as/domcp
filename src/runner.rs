use anyhow::{bail, Result};
use log::info;

use crate::cli::Args;
use crate::container::{Engine, RunConfig};
use crate::dockerfile::{self, Runner};
use crate::proxy::StdioProxy;

/// Main orchestration: detect runner → generate Dockerfile → build → run → proxy.
pub fn run(args: Args) -> Result<()> {
    // 1. Validate the command
    if args.command.is_empty() {
        bail!("No command provided. Usage: domcp -- uvx <mcp-server> [args...]");
    }

    let runner_name = &args.command[0];
    let runner = Runner::detect(runner_name)?;

    info!("Detected runner: {:?}", runner);
    info!("Command: {}", args.command.join(" "));

    // 2. Detect container engine
    let engine = Engine::detect(args.engine.as_deref())?;

    // 3. Generate Dockerfile
    let dockerfile_content = dockerfile::generate(runner, &args.command)?;

    // 4. Build container image
    let image_tag = engine.build_image(&dockerfile_content, &args.command, args.rebuild)?;

    // 5. Determine working directory
    let workdir = args
        .workdir
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    info!("Mounting workdir: {} → /work", workdir.display());

    // 6. Launch container
    let config = RunConfig {
        image: image_tag,
        workdir,
        extra_mounts: args.extra_mounts,
        envs: args.envs,
        network: args.network,
        ports: args.ports,
        user_map: args.user_map,
    };

    let child = engine.run_container(&config)?;

    // 7. Set up signal forwarding (Ctrl+C → container SIGTERM)
    crate::signal::setup_signal_forwarding(child.id());

    info!("Container started, proxying stdio...");

    // 7. Proxy stdio
    let proxy = StdioProxy::new(child);
    let exit_code = proxy.run()?;

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}
