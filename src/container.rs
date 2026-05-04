use anyhow::{bail, Context, Result};
use log::{debug, info};
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
        let output = Command::new("which")
            .arg(name)
            .output()
            .with_context(|| format!("Failed to look up `{name}`"))?;

        if !output.status.success() {
            bail!("`{name}` not found in PATH");
        }

        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let is_podman = name == "podman";

        info!("Using container engine: {} ({})", name, path);

        Ok(Self {
            name: name.to_string(),
            path: PathBuf::from(path),
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
        force_rebuild: bool,
    ) -> Result<String> {
        let tag = image_tag(command);

        // Check if image already exists
        if !force_rebuild && self.image_exists(&tag)? {
            info!("Image `{}` already exists, skipping build", tag);
            return Ok(tag);
        }

        info!("Building container image `{}`...", tag);

        let tmp = tempfile::Builder::new()
            .prefix("domcp-")
            .suffix(".Dockerfile")
            .tempfile()
            .context("Failed to create temp Dockerfile")?;

        std::fs::write(tmp.path(), dockerfile_content)
            .context("Failed to write Dockerfile")?;

        debug!("Dockerfile at: {}", tmp.path().display());

        let mut cmd = Command::new(&self.path);
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

    /// Check if an image with the given tag exists locally.
    fn image_exists(&self, tag: &str) -> Result<bool> {
        let output = Command::new(&self.path)
            .args(["image", "inspect", tag])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("Failed to check image existence")?;

        Ok(output.success())
    }

    /// Launch a container and return the child process with stdio connected.
    pub fn run_container(&self, config: &RunConfig) -> Result<std::process::Child> {
        let mut cmd = Command::new(&self.path);
        cmd.arg("run");

        // Always interactive with stdin attached for stdio-based MCP
        cmd.arg("-i");

        // Remove container after exit
        cmd.arg("--rm");

        // Network mode (only passed if explicitly requested)
        if let Some(net) = &config.network {
            cmd.args(["--network", net]);
        }

        // Mount working directory at the same path inside the container
        if let Some(ref workdir) = config.workdir {
            let workdir = workdir
                .canonicalize()
                .unwrap_or_else(|_| workdir.clone());
            let workdir_str = workdir.display().to_string();
            let mount_arg = format!("{w}:{w}", w = workdir_str);

            if self.is_podman {
                cmd.args(["-v", &format!("{mount_arg}:Z")]);
            } else {
                cmd.args(["-v", &mount_arg]);
            }

            // Set container working directory
            cmd.args(["-w", &workdir_str]);
        }

        // Extra mounts (each path mounted at the same location)
        for m in &config.extra_mounts {
            let p = m.canonicalize().unwrap_or_else(|_| m.clone());
            let ps = p.display().to_string();
            let mount = format!("{ps}:{ps}");
            if self.is_podman {
                cmd.args(["-v", &format!("{mount}:Z")]);
            } else {
                cmd.args(["-v", &mount]);
            }
        }

        // User mapping for rootless safety
        if config.user_map {
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

/// Configuration for running a container.
pub struct RunConfig {
    pub image: String,
    pub workdir: Option<PathBuf>,
    pub extra_mounts: Vec<PathBuf>,
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
        assert_eq!(image_tag(&cmd), "domcp/modelcontextprotocol-server-filesystem:latest");
    }
}
