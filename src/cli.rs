use clap::Parser;
use std::path::PathBuf;

/// domcp — Dockerize MCP servers for safer interaction.
///
/// Wraps any MCP server command (uvx, npx, pipx) in a container,
/// proxying stdio transparently. Only the current directory is
/// exposed to the wrapped server by default.
///
/// Usage:
///   domcp -- uvx some-mcp-server --arg1 --arg2
///   domcp -- npx -y @modelcontextprotocol/server-filesystem .
///   domcp --extra-mount /data -- uvx mcp-server
#[derive(Parser, Debug, Clone)]
#[command(
    name = "domcp",
    version,
    about = "Dockerize MCP servers for safer interaction"
)]
pub struct Args {
    /// Container engine to use (auto-detected if not set).
    /// Checks for podman first, then docker.
    #[arg(long, value_name = "ENGINE")]
    pub engine: Option<String>,

    /// Working directory to mount into the container.
    /// Mounted at the same path. Defaults to the current directory.
    /// Use --no-workdir to skip mounting entirely.
    #[arg(short = 'w', long, value_name = "DIR")]
    pub workdir: Option<PathBuf>,

    /// Don't mount any working directory into the container.
    #[arg(long)]
    pub no_workdir: bool,

    /// Additional directories to mount into the container.
    /// Each path is mounted at the same location inside the container.
    /// Can be specified multiple times.
    #[arg(short = 'v', long = "extra-mount", value_name = "PATH")]
    pub extra_mounts: Vec<PathBuf>,

    /// Host environment variables to expose to the container.
    /// Accepts literal names or prefixes ending with `*` (e.g. `ATLASSIAN_*`).
    /// Repeat the flag to allow multiple patterns.
    #[arg(short = 'e', long = "expose-env", value_name = "PATTERN")]
    pub expose_env: Vec<String>,

    /// Network mode for the container (e.g. "none", "host", "bridge").
    /// If not set, the container engine default is used.
    #[arg(long)]
    pub network: Option<String>,

    /// Additional packages to install in the container (via apk).
    /// Can be specified multiple times.
    #[arg(short = 'i', long = "install", value_name = "PKG")]
    pub packages: Vec<String>,

    /// Force rebuild of the container image even if it already exists.
    #[arg(long)]
    pub rebuild: bool,

    /// Port to expose from the container in HOST:CONTAINER format.
    /// Use this for SSE-based MCP servers instead of stdio.
    #[arg(short = 'p', long = "port", value_name = "HOST:CONTAINER")]
    pub ports: Vec<String>,

    /// Run the container as the current user (UID:GID).
    /// Enabled by default for rootless safety.
    #[arg(long, default_value_t = true)]
    pub user_map: bool,

    /// Don't map the current user into the container.
    #[arg(long)]
    pub no_user_map: bool,

    /// Print the generated Dockerfile and container run command, then exit.
    #[arg(long)]
    pub dry_run: bool,

    /// The command and arguments to run inside the container.
    /// Everything after `--` is treated as the wrapped command.
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn consumes_outer_double_dash_separator() {
        let args = Args::try_parse_from(["domcp", "--dry-run", "--", "uvx", "mcp-server-fetch"])
            .expect("arguments should parse");

        assert_eq!(args.command, vec!["uvx", "mcp-server-fetch"]);
    }

    #[test]
    fn preserves_inner_double_dash_for_wrapped_command() {
        let args = Args::try_parse_from([
            "domcp",
            "--dry-run",
            "--",
            "uvx",
            "mcp-server-fetch",
            "--",
            "--help",
        ])
        .expect("arguments should parse");

        assert_eq!(
            args.command,
            vec!["uvx", "mcp-server-fetch", "--", "--help"]
        );
    }

    #[test]
    fn expand_tilde_rewrites_bare_home_directory() {
        let home = dirs::home_dir().expect("home directory unavailable");

        assert_eq!(expand_tilde(PathBuf::from("~")), home);
    }

    #[test]
    fn expand_tilde_rewrites_home_relative_paths() {
        let home = dirs::home_dir().expect("home directory unavailable");

        assert_eq!(expand_tilde(PathBuf::from("~/repo")), home.join("repo"));
        assert_eq!(
            expand_tilde(PathBuf::from("~/.ssh/config")),
            home.join(".ssh").join("config")
        );
    }

    #[test]
    fn expand_tilde_leaves_non_tilde_paths_unchanged() {
        let path = PathBuf::from("/tmp/domcp-test-path");

        assert_eq!(expand_tilde(path.clone()), path);
    }

    #[test]
    fn expand_tilde_does_not_expand_other_users() {
        let path = PathBuf::from("~alice/project");

        assert_eq!(expand_tilde(path.clone()), path);
    }
}

pub fn parse() -> Args {
    let mut args = Args::parse();
    if args.no_user_map {
        args.user_map = false;
    }
    args.extra_mounts = args.extra_mounts.into_iter().map(expand_tilde).collect();
    if let Some(w) = args.workdir.take() {
        args.workdir = Some(expand_tilde(w));
    }
    args
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        dirs::home_dir().unwrap_or(p)
    } else if let Some(rest) = s.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => p,
        }
    } else {
        p
    }
}
