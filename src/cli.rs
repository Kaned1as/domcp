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
///   domcp -- npx -y @modelcontextprotocol/server-filesystem /work
///   domcp --extra-mount /data:/data -- uvx mcp-server
#[derive(Parser, Debug, Clone)]
#[command(
    name = "domcp",
    version,
    about = "Dockerize MCP servers for safer interaction"
)]
pub struct Args {
    /// Container engine to use (auto-detected if not set).
    /// Checks for podman first, then docker.
    #[arg(short = 'e', long, value_name = "ENGINE")]
    pub engine: Option<String>,

    /// Working directory to mount into the container as /work.
    /// Defaults to the current directory.
    #[arg(short = 'w', long, value_name = "DIR")]
    pub workdir: Option<PathBuf>,

    /// Additional bind mounts in SOURCE:TARGET format.
    /// Can be specified multiple times.
    #[arg(short = 'm', long = "extra-mount", value_name = "SRC:DST")]
    pub extra_mounts: Vec<String>,

    /// Additional environment variables to pass into the container.
    /// Format: KEY=VALUE. Can be specified multiple times.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub envs: Vec<String>,

    /// Network mode for the container (e.g. "none", "host", "bridge").
    /// Defaults to "none" for maximum isolation.
    #[arg(long, default_value = "none")]
    pub network: String,

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

pub fn parse() -> Args {
    let mut args = Args::parse();
    if args.no_user_map {
        args.user_map = false;
    }
    args
}
