use crate::packages;
use anyhow::{Context, Result, bail};
use log::{debug, info};
use serde::Deserialize;
use serde_json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};
use tokio::process::Command as TokioCommand;

/// Detected container engine with its binary path.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Engine {
    name: String,
    path: PathBuf,
    pub is_podman: bool,
}

impl Engine {
    /// Auto-detect the container engine, preferring podman.
    pub fn detect(preferred: Option<&str>) -> Result<Self> {
        if let Some(name) = preferred {
            return Self::find(name);
        }

        // Prefer podman for rootless-by-default safety
        if let Ok(engine) = Self::find("podman") {
            return Ok(engine);
        }

        if let Ok(engine) = Self::find("docker") {
            return Ok(engine);
        }

        bail!(
            "No container engine found. Install podman or docker.\n\
             See: https://podman.io/getting-started/installation"
        )
    }

    fn find(name: &str) -> Result<Self> {
        let path = which::which(name).with_context(|| format!("Failed to look up `{name}`"))?;
        let is_podman = name == "podman";

        info!("Using container engine: {} ({})", name, path.display());

        Ok(Self {
            name: name.to_string(),
            path,
            is_podman,
        })
    }

    /// Build a container image from a Dockerfile string.
    ///
    /// Returns the image tag.
    pub fn build_image(
        &self,
        dockerfile_content: &str,
        command: &[String],
        packages: &[String],
        force_rebuild: bool,
    ) -> Result<String> {
        let tag = image_tag(command);
        let requested_packages = packages::canonicalize(packages);

        if !force_rebuild {
            match self.image_package_state(&tag)? {
                ImagePackageState::NotFound => {}
                ImagePackageState::Present(existing) if existing == requested_packages => {
                    info!("Image `{}` already exists, skipping build", tag);
                    return Ok(tag);
                }
                ImagePackageState::Missing if requested_packages.is_empty() => {
                    info!("Image `{}` already exists, skipping build", tag);
                    return Ok(tag);
                }
                ImagePackageState::Present(existing) => {
                    info!("Image `{}` package list changed; rebuilding", tag);
                    debug!(
                        "Existing packages: {:?}, requested: {:?}",
                        existing, requested_packages
                    );
                }
                ImagePackageState::Missing => {
                    info!("Image `{}` missing package metadata; rebuilding", tag);
                    debug!("Requested packages: {:?}", requested_packages);
                }
            }
        }

        info!("Building container image `{}`...", tag);

        let tmp = tempfile::Builder::new()
            .prefix("domcp-")
            .suffix(".Dockerfile")
            .tempfile()
            .context("Failed to create temp Dockerfile")?;

        std::fs::write(tmp.path(), dockerfile_content).context("Failed to write Dockerfile")?;

        debug!("Dockerfile at: {}", tmp.path().display());

        let mut cmd = StdCommand::new(&self.path);
        cmd.arg("build")
            .arg("-f")
            .arg(tmp.path())
            .arg("-t")
            .arg(&tag)
            .arg(".")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().context("Failed to run container build")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Container image build failed:\n{}", stderr);
        }

        info!("Image `{}` built successfully", tag);
        Ok(tag)
    }

    fn image_package_state(&self, tag: &str) -> Result<ImagePackageState> {
        let output = StdCommand::new(&self.path)
            .args(["image", "inspect", tag])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("Failed to inspect image `{tag}`"))?;

        if !output.status.success() {
            return Ok(ImagePackageState::NotFound);
        }

        let records: Vec<InspectRecord> = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("Failed to parse inspect output for `{tag}`"))?;

        let record = match records.into_iter().next() {
            Some(record) => record,
            None => return Ok(ImagePackageState::NotFound),
        };

        match record.config.labels {
            Some(labels) => match labels.get(packages::PACKAGES_LABEL_KEY) {
                Some(value) => match packages::parse_label_value(value) {
                    Some(parsed) => Ok(ImagePackageState::Present(parsed)),
                    None => {
                        debug!("Image `{}` has invalid package metadata: {}", tag, value);
                        Ok(ImagePackageState::Missing)
                    }
                },
                None => Ok(ImagePackageState::Missing),
            },
            None => Ok(ImagePackageState::Missing),
        }
    }

    /// Append a bind mount to the container runtime command using the explicit
    /// `--mount` syntax.
    ///
    /// `cmd` is the in-progress container runtime command that will receive the
    /// extra `--mount` arguments.
    ///
    /// `mount` describes both sides of the bind:
    /// - `mount.host` is the host path that must be exposed to the container
    /// - `mount.container` is the Linux-style destination path inside the container
    ///
    /// Podman gets an extra SELinux relabel hint so mounted content remains
    /// accessible on SELinux-enabled hosts.
    fn add_bind_mount(&self, cmd: &mut TokioCommand, mount: &BindMount) {
        let mut spec = format!(
            "type=bind,src={},dst={}",
            mount.host.display(),
            mount.container
        );

        if self.is_podman {
            spec.push_str(",relabel=private");
        }

        cmd.args(["--mount", &spec]);
    }

    /// Apply host user mapping options for the current platform.
    ///
    /// `cmd` is the in-progress container runtime command.
    /// `config` provides the user's `--no-user-map` preference.
    ///
    /// On Unix, Podman uses `--userns=keep-id` and Docker receives an explicit
    /// `--user UID:GID`. On Windows we currently do nothing because Linux-style
    /// UID/GID mapping does not translate to the host platform.
    fn add_user_mapping(&self, cmd: &mut TokioCommand, config: &RunConfig) {
        if !config.user_map {
            return;
        }

        #[cfg(unix)]
        {
            if self.is_podman {
                // Podman: --userns=keep-id maps host UID to container UID=0
                // This is the cleanest rootless approach — the user inside
                // the container sees themselves as their host UID with proper
                // file ownership on mounted volumes.
                cmd.args(["--userns=keep-id"]);
            } else {
                // Docker: explicitly set UID:GID
                let uid = nix::unistd::getuid();
                let gid = nix::unistd::getgid();
                cmd.args(["--user", &format!("{uid}:{gid}")]);
            }
        }

        #[cfg(windows)]
        {
            let _ = cmd;
        }
    }

    /// Launch a container and return the child process with stdio connected.
    pub fn run_container(&self, config: &RunConfig) -> Result<tokio::process::Child> {
        let mut cmd = TokioCommand::new(&self.path);
        cmd.arg("run");

        // Always interactive with stdin attached for stdio-based MCP
        cmd.arg("-i");

        // Remove container after exit
        cmd.arg("--rm");

        // Network mode (only passed if explicitly requested)
        if let Some(net) = &config.network {
            cmd.args(["--network", net]);
        }

        // Mount working directory and set the container working directory.
        if let Some(ref workdir) = config.workdir {
            self.add_bind_mount(&mut cmd, workdir);
            cmd.args(["-w", &workdir.container]);
        }

        // Extra mounts.
        for mount in &config.extra_mounts {
            self.add_bind_mount(&mut cmd, mount);
        }

        // User mapping for rootless safety.
        self.add_user_mapping(&mut cmd, config);

        // Port mappings
        for p in &config.ports {
            cmd.args(["-p", p]);
        }

        // Environment variables
        for e in &config.envs {
            cmd.args(["-e", e]);
        }

        // Set hostname for easy identification
        cmd.args(["--hostname", "domcp"]);

        // Image and (no extra cmd — entrypoint is baked in)
        cmd.arg(&config.image);

        debug!("Container command: {:?}", cmd);

        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to start container")?;

        Ok(child)
    }
}

#[derive(Debug)]
enum ImagePackageState {
    NotFound,
    Missing,
    Present(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct InspectRecord {
    #[serde(rename = "Config")]
    config: InspectConfig,
}

#[derive(Debug, Deserialize)]
struct InspectConfig {
    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,
}

/// Bind mount specification between host and container paths.
#[derive(Debug, Clone)]
pub struct BindMount {
    pub host: PathBuf,
    pub container: String,
}

/// Configuration for running a container.
pub struct RunConfig {
    pub image: String,
    pub workdir: Option<BindMount>,
    pub extra_mounts: Vec<BindMount>,
    pub envs: Vec<String>,
    pub network: Option<String>,
    pub ports: Vec<String>,
    pub user_map: bool,
}

/// Generate an image tag from the command.
///
/// Uses the package name as a human-readable prefix with a `latest` tag.
fn image_tag(command: &[String]) -> String {
    let prefix = command
        .iter()
        .skip(1) // skip command (uvx/npx)
        .find(|a| !a.starts_with('-')) // skip command-line options (-y/-g)
        .map(|s| {
            s.strip_prefix('@') // remove leading @
                .unwrap_or(s)
                .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
                .to_lowercase()
        })
        .unwrap_or_else(|| "mcp".to_string());

    format!("domcp/{prefix}:latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_tag_format() {
        let cmd: Vec<String> = vec!["uvx".into(), "mcp-server-fetch".into()];
        assert_eq!(image_tag(&cmd), "domcp/mcp-server-fetch:latest");
    }

    #[test]
    fn test_image_tag_different_commands() {
        let cmd1: Vec<String> = vec!["uvx".into(), "mcp-server-fetch".into()];
        let cmd2: Vec<String> = vec!["uvx".into(), "mcp-server-git".into()];
        assert_ne!(image_tag(&cmd1), image_tag(&cmd2));
    }

    #[test]
    fn test_image_tag_scoped_npm() {
        let cmd: Vec<String> = vec![
            "npx".into(),
            "-y".into(),
            "@modelcontextprotocol/server-filesystem".into(),
        ];
        assert_eq!(
            image_tag(&cmd),
            "domcp/modelcontextprotocol-server-filesystem:latest"
        );
    }
}
