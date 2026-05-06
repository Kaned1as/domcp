use anyhow::{Context, Result, bail};
use log::{info, warn};
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

use crate::cli::Args;
use crate::container::{BindMount, Engine, RunConfig};
use crate::dockerfile::{self, Runner};
use crate::proxy::StdioProxy;
use crate::transport::{self, Transport};

/// Main orchestration: detect runner → generate Dockerfile → build → run → proxy.
pub async fn run(args: Args) -> Result<()> {
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

    // 3. Resolve environment variables to pass into the container
    let envs: Vec<String> = collect_exposed_host_env(&args.expose_env)
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    // 4. Detect transport mode (stdio vs HTTP/SSE)
    let detected_transport = transport::detect(&args.command, &envs);

    // Collect mutable copies of network / ports so we can adjust them
    let network: Option<String> = args.network.clone();
    let mut ports = args.ports.clone();

    if let Transport::Http { port } = &detected_transport {
        let port = *port;
        apply_http_transport(&network, &mut ports, port)?;
    }

    let extra_mounts = prepare_extra_mounts(&args.extra_mounts);

    // 5. Detect container engine
    let engine = Engine::detect(args.engine.as_deref())?;

    // 6. Generate Dockerfile (with EXPOSE for HTTP)
    let expose_port = match &detected_transport {
        Transport::Http { port } => Some(*port),
        Transport::Stdio => None,
    };
    let dockerfile_content =
        dockerfile::generate(runner, &args.command, expose_port, &args.packages)?;

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
        for (original, mapped) in args.extra_mounts.iter().zip(extra_mounts.iter()) {
            println!(
                "Extra mount: {} -> {}",
                original.display(),
                mapped.container.display()
            );
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
    let image_tag = engine.build_image(
        &dockerfile_content,
        &args.command,
        &args.packages,
        args.rebuild,
    )?;

    match &workdir {
        Some(w) => info!("Mounting workdir: {}", w.display()),
        None => info!("No workdir mounted"),
    }

    // 8. Launch container
    let config = RunConfig {
        image: image_tag,
        workdir,
        extra_mounts,
        envs,
        network,
        ports,
        user_map: args.user_map,
    };

    let child = engine.run_container(&config)?;

    // 9. Set up signal forwarding (Ctrl+C → container SIGTERM)
    let child_pid = child.id().context("Failed to get container process ID")?;
    crate::signal::setup_signal_forwarding(child_pid);

    // 10. Run in the appropriate mode
    match detected_transport {
        Transport::Stdio => {
            info!("Container started, proxying stdio...");
            let proxy = StdioProxy::new(child);
            let exit_code = proxy.run().await?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Transport::Http { port } => {
            info!("Container started in HTTP mode");
            info!("MCP server available at: http://localhost:{port}");
            let exit_code = wait_http_container(child).await?;
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
async fn wait_http_container(mut child: tokio::process::Child) -> Result<i32> {
    // Forward stderr in the background
    let stderr_task = child.stderr.take().map(|mut container_stderr| {
        tokio::spawn(async move {
            forward_stream_to_stderr("container→stderr", &mut container_stderr).await;
        })
    });

    // Also drain stdout (server might print startup messages there)
    let stdout_task = child.stdout.take().map(|mut container_stdout| {
        tokio::spawn(async move {
            forward_stream_to_stderr("container→stdout", &mut container_stdout).await;
        })
    });

    let status = child.wait().await.context("Failed to wait for container")?;

    if let Some(task) = stderr_task {
        await_task("container→stderr", task).await;
    }
    if let Some(task) = stdout_task {
        await_task("container→stdout", task).await;
    }

    Ok(status.code().unwrap_or(1))
}

async fn forward_stream_to_stderr<R>(label: &str, reader: &mut R)
where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; 4096];
    let mut stderr = io::stderr();

    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = stderr.write_all(&buf[..n]).await {
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        warn!("{label}: write error: {e}");
                    }
                    break;
                }
                if let Err(e) = stderr.flush().await {
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        warn!("{label}: flush error: {e}");
                    }
                    break;
                }
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::BrokenPipe {
                    warn!("{label}: read error: {e}");
                }
                break;
            }
        }
    }
}

async fn await_task(label: &str, task: JoinHandle<()>) {
    match task.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {}
        Err(e) => warn!("{label}: task join error: {e}"),
    }
}

fn collect_exposed_host_env(patterns: &[String]) -> Vec<(String, String)> {
    if patterns.is_empty() {
        return Vec::new();
    }

    std::env::vars()
        .filter(|(key, _)| {
            patterns
                .iter()
                .any(|pattern| env_pattern_matches(pattern, key))
        })
        .collect()
}

fn env_pattern_matches(pattern: &str, key: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        key.starts_with(prefix)
    } else {
        pattern == key
    }
}

fn prepare_extra_mounts(mounts: &[std::path::PathBuf]) -> Vec<BindMount> {
    let home = dirs::home_dir();

    // translate ~/dir to /opt/home/dir inside the container
    mounts
        .iter()
        .map(|mount| {
            let host = mount.canonicalize().unwrap_or_else(|_| mount.clone());

            let mut container = host.clone();
            if let Some(home_dir) = home.as_ref() {
                if let Ok(rel) = host.strip_prefix(home_dir) {
                    container = std::path::PathBuf::from("/opt/home").join(rel);
                }
            }

            BindMount { host, container }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_pattern_matches_exact() {
        assert!(env_pattern_matches("FOO", "FOO"));
        assert!(!env_pattern_matches("FOO", "FOOBAR"));
    }

    #[test]
    fn env_pattern_matches_prefix() {
        assert!(env_pattern_matches("ATLASSIAN_*", "ATLASSIAN_TOKEN"));
        assert!(!env_pattern_matches("ATLASSIAN_*", "GITHUB_TOKEN"));
    }

    #[test]
    fn collect_exposed_host_env_respects_patterns() {
        let key = "DOMCP_TEST_ENV_COLLECT";
        let value = "value123";
        unsafe {
            std::env::set_var(key, value);
        }

        let patterns = vec!["DOMCP_TEST_ENV_*".to_string(), key.to_string()];
        let collected = collect_exposed_host_env(&patterns);

        unsafe {
            std::env::remove_var(key);
        }

        let matches: Vec<_> = collected.iter().filter(|(k, _)| k == key).collect();
        assert_eq!(1, matches.len());
        assert!(matches.iter().any(|(_, v)| v == value));
    }

    #[test]
    fn collect_exposed_host_env_empty_without_patterns() {
        assert!(collect_exposed_host_env(&[]).is_empty());
    }

    #[test]
    fn prepare_extra_mounts_rewrites_home_relative_paths() {
        let home = dirs::home_dir().expect("home directory unavailable");
        let mount = home.join(".ssh");
        let mounts = vec![mount.clone()];

        let prepared = prepare_extra_mounts(&mounts);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].host, mount);
        assert_eq!(
            prepared[0].container,
            std::path::Path::new("/opt/home").join(".ssh")
        );
    }

    #[test]
    fn prepare_extra_mounts_passthrough_outside_home() {
        let mount = std::path::PathBuf::from("/tmp/domcp-non-home");
        let mounts = vec![mount.clone()];

        let prepared = prepare_extra_mounts(&mounts);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].host, mount);
        assert_eq!(prepared[0].container, mount);
    }
}
