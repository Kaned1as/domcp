use anyhow::{bail, Result};
use log::info;
use std::path::Path;

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

    // 2. Determine working directory early (needed for path rewriting)
    let workdir = args
        .workdir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));
    let workdir = workdir
        .canonicalize()
        .unwrap_or(workdir);

    // 3. Rewrite host paths in the command to container paths.
    //    e.g. "/home/user/project" → "/work" if that's the mounted workdir.
    let command = rewrite_paths(&args.command, &workdir, &args.extra_mounts);

    // 4. Detect container engine
    let engine = Engine::detect(args.engine.as_deref())?;

    // 5. Generate Dockerfile
    let dockerfile_content = dockerfile::generate(runner, &command)?;

    // Dry-run: print the Dockerfile and exit
    if args.dry_run {
        println!("=== Generated Dockerfile ===");
        println!("{}", dockerfile_content);
        println!("=== Workdir mount ===");
        println!("{} → /work", workdir.display());
        for m in &args.extra_mounts {
            println!("Extra mount: {}", m);
        }
        println!("\n=== Container command ===");
        println!("{:?}", command);
        return Ok(());
    }

    // 6. Build container image
    let image_tag = engine.build_image(&dockerfile_content, &command, args.rebuild)?;

    info!("Mounting workdir: {} → /work", workdir.display());

    // 7. Launch container
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

    // 8. Set up signal forwarding (Ctrl+C → container SIGTERM)
    crate::signal::setup_signal_forwarding(child.id());

    info!("Container started, proxying stdio...");

    // 9. Proxy stdio
    let proxy = StdioProxy::new(child);
    let exit_code = proxy.run()?;

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}

/// Rewrite host filesystem paths in command arguments to their container equivalents.
///
/// If a command argument looks like an absolute path that matches the mounted workdir
/// or an extra mount, it gets translated to the container mount point.
///
/// Example: workdir=/home/user/project → /work
///   Command: ["uvx", "mcp-server-fs", "/home/user/project"]
///   Becomes: ["uvx", "mcp-server-fs", "/work"]
fn rewrite_paths(command: &[String], workdir: &Path, extra_mounts: &[String]) -> Vec<String> {
    // Build a map of host_path → container_path
    let mut path_map: Vec<(String, String)> = Vec::new();

    // The primary workdir mount
    let workdir_str = workdir.to_string_lossy().to_string();
    path_map.push((workdir_str, "/work".to_string()));

    // Extra mounts: "SOURCE:TARGET" format
    for mount in extra_mounts {
        if let Some((src, dst)) = mount.split_once(':') {
            let src_canonical = Path::new(src)
                .canonicalize()
                .unwrap_or_else(|_| Path::new(src).to_path_buf());
            path_map.push((src_canonical.to_string_lossy().to_string(), dst.to_string()));
        }
    }

    // Sort by path length descending so more specific paths match first
    path_map.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    command
        .iter()
        .map(|arg| {
            // Skip the runner itself and flags
            if arg.starts_with('-') || !arg.starts_with('/') {
                return arg.clone();
            }

            // Try to match against our mount map
            for (host, container) in &path_map {
                if arg == host {
                    info!("Path rewrite: {} → {}", arg, container);
                    return container.clone();
                }
                if let Some(suffix) = arg.strip_prefix(host) {
                    if suffix.starts_with('/') {
                        let rewritten = format!("{}{}", container, suffix);
                        info!("Path rewrite: {} → {}", arg, rewritten);
                        return rewritten;
                    }
                }
            }

            arg.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_rewrite_workdir() {
        let cmd = vec![
            "uvx".to_string(),
            "mcp-server-fs".to_string(),
            "/home/user/project".to_string(),
        ];
        let workdir = PathBuf::from("/home/user/project");
        let result = rewrite_paths(&cmd, &workdir, &[]);
        assert_eq!(result[2], "/work");
    }

    #[test]
    fn test_rewrite_subpath() {
        let cmd = vec![
            "uvx".to_string(),
            "mcp-server-fs".to_string(),
            "/home/user/project/src".to_string(),
        ];
        let workdir = PathBuf::from("/home/user/project");
        let result = rewrite_paths(&cmd, &workdir, &[]);
        assert_eq!(result[2], "/work/src");
    }

    #[test]
    fn test_no_rewrite_for_flags() {
        let cmd = vec![
            "uvx".to_string(),
            "--verbose".to_string(),
            "mcp-server".to_string(),
        ];
        let workdir = PathBuf::from("/home/user/project");
        let result = rewrite_paths(&cmd, &workdir, &[]);
        assert_eq!(result, cmd);
    }

    #[test]
    fn test_rewrite_extra_mount() {
        let cmd = vec![
            "uvx".to_string(),
            "mcp-server-fs".to_string(),
            "/data/files/doc.txt".to_string(),
        ];
        let workdir = PathBuf::from("/home/user/project");
        // Note: canonicalize won't work on non-existent paths in tests,
        // so we just test the logic with paths as-is
        let mounts = vec!["/data/files:/mnt/data".to_string()];
        let result = rewrite_paths(&cmd, &workdir, &mounts);
        // /data/files/doc.txt → /mnt/data/doc.txt
        assert_eq!(result[2], "/mnt/data/doc.txt");
    }
}
