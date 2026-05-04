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
    let workdir = if args.no_workdir {
        None
    } else {
        let w = args
            .workdir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));
        Some(w.canonicalize().unwrap_or(w))
    };

    // 3. Detect transport mode (stdio vs HTTP/SSE)
    let detected_transport = transport::detect(&args.command, &args.envs);

    // Collect mutable copies of network / ports / envs so we can adjust them
    let network: Option<String> = args.network.clone();
    let mut ports = args.ports.clone();

    // Build environment: host vars (unless --no-env) + explicit --env on top
    let mut envs: Vec<String> = if args.no_env {
        Vec::new()
    } else {
        std::env::vars()
            .filter(|(k, _)| !is_system_env(k))
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    };
    // Explicit --env values override/append
    envs.extend(args.envs.clone());

    if let Transport::Http { port } = &detected_transport {
        let port = *port;
        apply_http_transport(&network, &mut ports, port)?;
    }

    // 5. Detect container engine
    let engine = Engine::detect(args.engine.as_deref())?;

    // 6. Generate Dockerfile (with EXPOSE for HTTP)
    let expose_port = match &detected_transport {
        Transport::Http { port } => Some(*port),
        Transport::Stdio => None,
    };
    let dockerfile_content = dockerfile::generate(runner, &args.command, expose_port)?;

    // Dry-run: print the Dockerfile and exit
    if args.dry_run {
        println!("=== Detected transport ===");
        println!("{:?}", detected_transport);
        println!();
        println!("=== Generated Dockerfile ===");
        println!("{}", dockerfile_content);
        println!("=== Workdir ===");
        match &workdir {
            Some(w) => println!("{}", w.display()),
            None => println!("(none)"),
        }
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
        println!("{}", network.as_deref().unwrap_or("(engine default)"));
        println!("\n=== Container command ===");
        println!("{:?}", args.command);
        return Ok(());
    }

    // 7. Build container image
    let image_tag = engine.build_image(&dockerfile_content, &args.command, args.rebuild)?;

    match &workdir {
        Some(w) => info!("Mounting workdir: {}", w.display()),
        None => info!("No workdir mounted"),
    }

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
/// - Errors if network is "none" (incompatible with port mapping)
/// - Adds the port mapping if not already present
fn apply_http_transport(
    network: &Option<String>,
    ports: &mut Vec<String>,
    port: u16,
) -> Result<()> {
    if network.as_deref() == Some("none") {
        bail!(
            "--network none is incompatible with HTTP transport (port {port}).\n\
             Remove --network none or use --network bridge."
        );
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

    Ok(())
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

/// System-level environment variables that should not be forwarded
/// into the container (they are set by the container's own OS image).
const SYSTEM_ENV_VARS: &[&str] = &[
    "_",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HOME",
    "LS_COLORS",
    "HOSTNAME",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_NUMERIC",
    "LC_TIME",
    "LOGNAME",
    "MAIL",
    "OLDPWD",
    "PATH",
    "PWD",
    "SHELL",
    "SHLVL",
    "TERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "USER",
    "USERNAME",
    "WAYLAND_DISPLAY",
    "WINDOWID",
    "XAUTHORITY",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_DIRS",
    "XDG_MENU_PREFIX",
    "XDG_RUNTIME_DIR",
    "XDG_SEAT",
    "XDG_SESSION_CLASS",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_ID",
    "XDG_SESSION_TYPE",
    "XDG_VTNR",
];

/// Returns true if the variable name is a system-level env var that
/// should not be forwarded into the container.
fn is_system_env(key: &str) -> bool {
    SYSTEM_ENV_VARS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_env_filtered() {
        assert!(is_system_env("PATH"));
        assert!(is_system_env("SHELL"));
        assert!(is_system_env("HOSTNAME"));
        assert!(is_system_env("TERM"));
        assert!(is_system_env("LANG"));
        assert!(is_system_env("HOME"));
    }

    #[test]
    fn test_non_system_env_passed() {
        assert!(!is_system_env("API_KEY"));
        assert!(!is_system_env("GITHUB_TOKEN"));
        assert!(!is_system_env("AWS_ACCESS_KEY_ID"));
        assert!(!is_system_env("OPENAI_API_KEY"));
    }
}
