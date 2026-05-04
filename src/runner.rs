use anyhow::{bail, Context, Result};
use log::info;

use crate::cli::Args;
use crate::container::{Engine, RunConfig};
use crate::dockerfile::{self, Runner};
use crate::proxy::StdioProxy;
use crate::transport::{self, Transport};

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

    // 2. Determine working directory
    let workdir = args
        .workdir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));
    let workdir = workdir.canonicalize().unwrap_or(workdir);

    // 3. Copy command (may be mutated for HTTP transport)
    let mut command = args.command.clone();

    // 4. Detect transport mode (stdio vs HTTP/SSE)
    let detected_transport = transport::detect(&command, &args.envs);

    // Collect mutable copies of network / ports so we can adjust them for HTTP
    let mut network = args.network.clone();
    let mut ports = args.ports.clone();
    let mut envs = args.envs.clone();

    if let Transport::Http { port } = &detected_transport {
        let port = *port;
        apply_http_transport(&mut command, &mut network, &mut ports, &mut envs, port);
    }

    // 5. Detect container engine
    let engine = Engine::detect(args.engine.as_deref())?;

    // 6. Generate Dockerfile (with EXPOSE for HTTP)
    let expose_port = match &detected_transport {
        Transport::Http { port } => Some(*port),
        Transport::Stdio => None,
    };
    let dockerfile_content = dockerfile::generate(runner, &command, expose_port)?;

    // Dry-run: print the Dockerfile and exit
    if args.dry_run {
        println!("=== Detected transport ===");
        println!("{:?}", detected_transport);
        println!();
        println!("=== Generated Dockerfile ===");
        println!("{}", dockerfile_content);
        println!("=== Workdir ===");
        println!("{}", workdir.display());
        for m in &args.extra_mounts {
            println!("Extra mount: {}", m.display());
        }
        if !ports.is_empty() {
            println!("\n=== Port mappings ===");
            for p in &ports {
                println!("  {}", p);
            }
        }
        println!("\n=== Network ===");
        println!("{}", network);
        println!("\n=== Container command ===");
        println!("{:?}", command);
        return Ok(());
    }

    // 7. Build container image
    let image_tag = engine.build_image(&dockerfile_content, &command, args.rebuild)?;

    info!("Mounting workdir: {}", workdir.display());

    // 8. Launch container
    let config = RunConfig {
        image: image_tag,
        workdir,
        extra_mounts: args.extra_mounts,
        envs,
        network,
        ports,
        user_map: args.user_map,
    };

    let child = engine.run_container(&config)?;

    // 9. Set up signal forwarding (Ctrl+C → container SIGTERM)
    crate::signal::setup_signal_forwarding(child.id());

    // 10. Run in the appropriate mode
    match detected_transport {
        Transport::Stdio => {
            info!("Container started, proxying stdio...");
            let proxy = StdioProxy::new(child);
            let exit_code = proxy.run()?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Transport::Http { port } => {
            info!("Container started in HTTP mode");
            info!("MCP server available at: http://localhost:{port}");
            let exit_code = wait_http_container(child)?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
    }

    Ok(())
}

/// Apply adjustments required for HTTP transport mode.
///
/// - Ensures the server binds on 0.0.0.0 (needed for container port forwarding)
/// - Upgrades network from "none" to "bridge" (ports require network)
/// - Adds the port mapping if not already present
fn apply_http_transport(
    command: &mut Vec<String>,
    network: &mut String,
    ports: &mut Vec<String>,
    _envs: &mut Vec<String>,
    port: u16,
) {
    // Make the server bind on all interfaces inside the container
    transport::ensure_bind_all(command);

    // Ports require network — upgrade "none" to "bridge" automatically
    if network == "none" {
        info!("Upgrading network from 'none' to 'bridge' (required for port mapping)");
        *network = "bridge".to_string();
    }

    // Add the port mapping unless the user already supplied one for this port
    let port_str = port.to_string();
    let already_mapped = ports.iter().any(|p| {
        // Match "PORT:PORT", "PORT", or anything ending with ":PORT"
        p == &port_str || p == &format!("{port}:{port}") || p.ends_with(&format!(":{port}"))
    });

    if !already_mapped {
        let mapping = format!("{port}:{port}");
        info!("Auto-adding port mapping: {}", mapping);
        ports.push(mapping);
    }
}

/// Wait for an HTTP-mode container to exit, forwarding only stderr to the host.
///
/// In HTTP mode there's no stdin/stdout proxy — the MCP client talks to the
/// server over HTTP directly. We still forward stderr so error messages and
/// logs from the server are visible.
fn wait_http_container(mut child: std::process::Child) -> Result<i32> {
    use std::io::{self, Read, Write};

    // Forward stderr in a background thread
    let stderr = child.stderr.take();
    let stderr_thread = if let Some(mut container_stderr) = stderr {
        Some(
            std::thread::Builder::new()
                .name("http-stderr".to_string())
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        match container_stderr.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = io::stderr().write_all(&buf[..n]);
                                let _ = io::stderr().flush();
                            }
                            Err(_) => break,
                        }
                    }
                })
                .context("Failed to spawn stderr thread")?,
        )
    } else {
        None
    };

    // Also drain stdout (server might print startup messages there)
    let stdout = child.stdout.take();
    let stdout_thread = if let Some(mut container_stdout) = stdout {
        Some(
            std::thread::Builder::new()
                .name("http-stdout".to_string())
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        match container_stdout.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = io::stderr().write_all(&buf[..n]);
                                let _ = io::stderr().flush();
                            }
                            Err(_) => break,
                        }
                    }
                })
                .context("Failed to spawn stdout drain thread")?,
        )
    } else {
        None
    };

    let status = child.wait().context("Failed to wait for container")?;

    if let Some(t) = stderr_thread {
        let _ = t.join();
    }
    if let Some(t) = stdout_thread {
        let _ = t.join();
    }

    Ok(status.code().unwrap_or(1))
}

