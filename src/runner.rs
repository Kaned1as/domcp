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
        Some(prepare_bind_mount(&w))
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
            Some(w) => println!("{} -> {}", w.host.display(), w.container),
            None => println!("(none)"),
        }
        for (original, mapped) in args.extra_mounts.iter().zip(extra_mounts.iter()) {
            println!(
                "Extra mount: {} -> {}",
                original.display(),
                mapped.container
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
        Some(w) => info!("Mounting workdir: {} -> {}", w.host.display(), w.container),
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

    // 9. Set up shutdown listening so the child owner can terminate the container directly.
    let shutdown_rx = crate::signal::shutdown_channel();

    // 10. Run in the appropriate mode
    match detected_transport {
        Transport::Stdio => {
            info!("Container started, proxying stdio...");
            let proxy = StdioProxy::new(child, shutdown_rx);
            let exit_code = proxy.run().await?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Transport::Http { port } => {
            info!("Container started in HTTP mode");
            info!("MCP server available at: http://localhost:{port}");
            let exit_code = wait_http_container(child, shutdown_rx).await?;
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
async fn wait_http_container(
    mut child: tokio::process::Child,
    mut shutdown_rx: tokio::sync::mpsc::UnboundedReceiver<&'static str>,
) -> Result<i32> {
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

    let status = tokio::select! {
        status = child.wait() => {
            status.context("Failed to wait for container")?
        }
        shutdown = shutdown_rx.recv() => {
            if let Some(reason) = shutdown {
                info!("Received {reason}, terminating container process...");
                child
                    .start_kill()
                    .context("Failed to terminate container process")?;
            }
            child
                .wait()
                .await
                .context("Failed to wait for container after shutdown")?
        }
    };

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

/// Convert user-provided extra mount paths into fully prepared bind-mount
/// specifications.
///
/// `mounts` contains the host paths requested through CLI flags such as
/// `--extra-mount`. Each path is canonicalized when possible and translated to a
/// container destination path via [`prepare_bind_mount`].
fn prepare_extra_mounts(mounts: &[std::path::PathBuf]) -> Vec<BindMount> {
    mounts
        .iter()
        .map(|mount| prepare_bind_mount(mount))
        .collect()
}

/// Build a single bind-mount specification from a host path.
///
/// `path` is the host-side path that should become visible inside the
/// container. The function canonicalizes it when possible, preserves the host
/// path in [`BindMount::host`], and computes a Linux-compatible in-container
/// destination in [`BindMount::container`].
fn prepare_bind_mount(path: &std::path::Path) -> BindMount {
    let host = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let container = map_container_path(&host);
    BindMount { host, container }
}

/// Map a host path to the path that should be used inside the container.
///
/// On non-Windows hosts we keep the path unchanged so MCP servers continue to
/// see the same absolute paths they would see outside the container.
///
/// `host` is the canonical host path that will be bind-mounted.
#[cfg(not(windows))]
fn map_container_path(host: &std::path::Path) -> String {
    host.to_string_lossy().into_owned()
}

/// Map a Windows host path to a stable Linux-style container path.
///
/// Exact absolute-path identity is impossible for Linux containers on Windows,
/// so this function preserves the path shape instead:
/// - paths inside the user's home directory become `/opt/home/...`
/// - other paths become `/opt/host/<drive-or-prefix>/...`
///
/// `host` is the canonical Windows path that will be bind-mounted.
#[cfg(windows)]
fn map_container_path(host: &std::path::Path) -> String {
    let home = dirs::home_dir();
    if let Some(home_dir) = home.as_ref() {
        if let Ok(relative) = host.strip_prefix(home_dir) {
            return join_container_path("/opt/home", relative);
        }
    }

    use std::path::{Component, Prefix};

    let mut prefix_segments = vec!["opt".to_string(), "host".to_string()];
    let mut components = host.components();

    if let Some(Component::Prefix(prefix)) = components.next() {
        match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                prefix_segments.push(char::from(drive).to_ascii_lowercase().to_string());
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                prefix_segments.push("unc".to_string());
                prefix_segments.push(server.to_string_lossy().into_owned());
                prefix_segments.push(share.to_string_lossy().into_owned());
            }
            Prefix::Verbatim(path) | Prefix::DeviceNS(path) => {
                prefix_segments.push(path.to_string_lossy().into_owned());
            }
        }
    }

    let mut container = String::new();
    for segment in prefix_segments {
        container.push('/');
        container.push_str(&segment);
    }

    for component in components {
        if let Component::Normal(part) = component {
            container.push('/');
            container.push_str(&part.to_string_lossy());
        }
    }

    if container.is_empty() {
        "/opt/host".to_string()
    } else {
        container
    }
}

/// Join a Linux-style container base path with a relative host path fragment.
///
/// `base` is the already-chosen container prefix such as `/opt/home`.
/// `relative` is the path segment relative to that prefix on the host side.
/// Only normal path components are appended, producing a slash-separated path
/// that is valid inside Linux containers.
#[cfg(windows)]
fn join_container_path(base: &str, relative: &std::path::Path) -> String {
    use std::path::Component;

    let mut container = base.trim_end_matches('/').to_string();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            container.push('/');
            container.push_str(&part.to_string_lossy());
        }
    }
    container
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

    #[cfg(not(windows))]
    #[test]
    fn prepare_extra_mounts_preserve_unix_paths() {
        let home = dirs::home_dir().expect("home directory unavailable");
        let mount = home.join(".ssh");
        let mounts = vec![mount.clone()];

        let prepared = prepare_extra_mounts(&mounts);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].host, mount);
        assert_eq!(
            prepared[0].container,
            prepared[0].host.display().to_string()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn prepare_bind_mount_keeps_home_relative_shape_after_tilde_expansion() {
        let home = dirs::home_dir().expect("home directory unavailable");
        let expanded = home.join("projects").join("domcp");

        let prepared = prepare_bind_mount(&expanded);

        assert_eq!(prepared.host, expanded);
        assert_eq!(prepared.container, prepared.host.display().to_string());
    }

    #[cfg(not(windows))]
    #[test]
    fn prepare_extra_mounts_passthrough_outside_home() {
        let mount = std::path::PathBuf::from("/tmp/domcp-non-home");
        let mounts = vec![mount.clone()];

        let prepared = prepare_extra_mounts(&mounts);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].host, mount);
        assert_eq!(
            prepared[0].container,
            prepared[0].host.display().to_string()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_paths_map_under_opt_home() {
        let home = dirs::home_dir().expect("home directory unavailable");
        let mount = home.join("Documents").join("repo");

        assert_eq!(map_container_path(&mount), "/opt/home/Documents/repo");
    }

    #[cfg(windows)]
    #[test]
    fn windows_prepared_home_mount_matches_tilde_expanded_layout() {
        let home = dirs::home_dir().expect("home directory unavailable");
        let expanded = home.join("projects").join("domcp");

        let prepared = prepare_bind_mount(&expanded);

        assert_eq!(prepared.host, expanded);
        assert_eq!(prepared.container, "/opt/home/projects/domcp");
    }

    #[cfg(windows)]
    #[test]
    fn windows_non_home_paths_map_under_opt_host() {
        let mount = std::path::PathBuf::from(r"D:\work\repo");

        assert_eq!(map_container_path(&mount), "/opt/host/d/work/repo");
    }
}
