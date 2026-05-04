# domcp — Dockerize MCP Servers

**domcp** wraps any [MCP](https://modelcontextprotocol.io/) server command (`uvx`, `npx`, `pipx`) in a container, proxying stdio transparently. This makes MCP interaction safer by isolating the server from your host system — only the current working directory is exposed by default.

## Why?

MCP servers often get broad filesystem access, network access, and run under your user account. A misbehaving or compromised server can read your SSH keys, browser cookies, or any file on your system. **domcp** fixes this by:

- 🐳 Running the MCP server in a container with **only `/work`** (your current directory) mounted
- 🔒 **No network access** by default (`--network=none`)
- 👤 **Rootless** by default — runs as your UID:GID, not root
- 🦭 **Podman-first** — prefers podman for rootless-by-default safety
- ⚡ **Zero config** — auto-detects everything, just prefix your command with `domcp --`

## Installation

```bash
cargo install --path .
```

Or build from source:

```bash
cargo build --release
# Binary at target/release/domcp
```

### Prerequisites

- [Podman](https://podman.io/getting-started/installation) (preferred) **or** Docker
- That's it — domcp generates Dockerfiles and builds images automatically

## Usage

Just prefix your MCP server command with `domcp --`:

```bash
# Wrap a uvx-based MCP server
domcp -- uvx mcp-server-fetch

# Wrap an npx-based MCP server
domcp -- npx -y @modelcontextprotocol/server-filesystem /work

# Wrap a pipx-based MCP server
domcp -- pipx run mcp-server-time
```

### MCP Client Configuration

Use domcp in your MCP client config (e.g. Claude Desktop `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "fetch": {
      "command": "domcp",
      "args": ["--", "uvx", "mcp-server-fetch"]
    },
    "filesystem": {
      "command": "domcp",
      "args": [
        "--", "npx", "-y",
        "@modelcontextprotocol/server-filesystem", "/work"
      ]
    }
  }
}
```

### Options

```
domcp [OPTIONS] -- <COMMAND>...

Options:
  -e, --engine <ENGINE>       Container engine (auto-detected: podman > docker)
  -w, --workdir <DIR>         Directory to mount as /work (default: $PWD)
  -m, --extra-mount <SRC:DST> Additional bind mounts (repeatable)
      --env <KEY=VALUE>       Environment variables for the container (repeatable)
      --network <MODE>        Network mode: none, host, bridge (default: none)
      --rebuild               Force rebuild of the container image
  -p, --port <HOST:CONTAINER> Port mappings for SSE-based servers (repeatable)
      --no-user-map           Don't map current user into container
  -h, --help                  Print help
  -V, --version               Print version
```

### Examples

```bash
# Allow network access (needed for servers that fetch from the internet)
domcp --network=host -- uvx mcp-server-fetch

# Mount additional directories
domcp -m /path/to/data:/data -- uvx mcp-server-filesystem /work /data

# Use docker instead of podman
domcp -e docker -- npx -y @modelcontextprotocol/server-everything

# SSE-based server with port mapping
domcp --network=bridge -p 8080:8080 -- uvx mcp-server-sse --port 8080

# Pass environment variables
domcp --env API_KEY=secret -- uvx mcp-server-github

# Force image rebuild
domcp --rebuild -- uvx mcp-server-fetch
```

## How It Works

1. **Detect** the runner type from the command (`uvx` → Python/uv, `npx` → Node.js, `pipx` → Python/pipx)
2. **Generate** an optimized Dockerfile with the MCP server package pre-installed
3. **Build** the container image (cached with deterministic tags — only rebuilds when the command changes)
4. **Launch** the container with minimal permissions:
   - Only your working directory mounted at `/work`
   - Network disabled
   - Running as your UID:GID
5. **Proxy** stdin/stdout/stderr bidirectionally between your MCP client and the containerized server
6. **Forward** signals (Ctrl+C → SIGTERM) for graceful shutdown

## Security Model

| Threat | Mitigation |
|--------|-----------|
| Filesystem access | Only `/work` (CWD) is mounted; rest of host is invisible |
| Network exfiltration | `--network=none` by default |
| Privilege escalation | Runs as your UID:GID, not root |
| Container escape | Podman is rootless by default; no `--privileged` |
| Supply chain (image) | Images built locally from official base images |
| SELinux bypass | Podman mounts use `:Z` for proper relabeling |

## License

MIT
