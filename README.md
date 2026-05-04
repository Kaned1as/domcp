# domcp — Dockerize MCP Servers

**domcp** wraps any [MCP](https://modelcontextprotocol.io/) server command (`uvx`, `npx`, `pipx`) in a container, proxying stdio transparently. This makes MCP interaction safer by isolating the server from your host system — only the current working directory is exposed by default.

## Why?

MCP servers often get broad filesystem access, network access, and run under your user account. A misbehaving or compromised server can read your SSH keys, browser cookies, or any file on your system. **domcp** fixes this by:

- 🐳 Running the MCP server in a container with **only `/work`** (your current directory) mounted
- 🔒 **No network access** by default (`--network=none`)
- 👤 **Rootless** by default — runs as your UID:GID, not root
- 🦭 **Podman-first** — prefers podman for rootless-by-default safety
- ⚡ **Zero config** — auto-detects everything, just prefix your command with `domcp --`
- 🌐 **Auto HTTP/SSE detection** — detects `--transport sse`, `--port`, env vars, and auto-configures port mapping + network

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

# HTTP/SSE server — transport and port are detected automatically
domcp -- uvx mcp-server-sse --transport sse --port 8080
# → auto-exposes port 8080, sets network=bridge, binds 0.0.0.0
```

### MCP Client Configuration

Use domcp in your MCP client config (e.g. Claude Desktop `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "fetch": {
      "command": "domcp",
      "args": ["--network", "host", "--", "uvx", "mcp-server-fetch"]
    },
    "filesystem": {
      "command": "domcp",
      "args": [
        "--", "npx", "-y",
        "@modelcontextprotocol/server-filesystem", "/work"
      ]
    },
    "sse-server": {
      "url": "http://localhost:8080/sse",
      "command": "domcp",
      "args": [
        "--", "uvx", "mcp-server-sse",
        "--transport", "sse", "--port", "8080"
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
  -p, --port <HOST:CONTAINER> Port mappings (repeatable; auto-added for HTTP servers)
      --no-user-map           Don't map current user into container
      --dry-run               Print Dockerfile and config without building/running
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

# SSE/HTTP server — port and network auto-detected from --transport/--port
domcp -- uvx mcp-server-sse --transport sse --port 8080

# Same but via environment variables
domcp --env MCP_TRANSPORT=sse --env PORT=8080 -- uvx mcp-server-sse

# Override auto-detected port mapping
domcp -p 9090:8080 -- uvx mcp-server-sse --transport sse --port 8080

# Pass environment variables
domcp --env API_KEY=secret -- uvx mcp-server-github

# Force image rebuild
domcp --rebuild -- uvx mcp-server-fetch
```

## How It Works

1. **Detect** the runner type from the command (`uvx` → Python/uv, `npx` → Node.js, `pipx` → Python/pipx)
2. **Detect transport** — scan command flags (`--transport`, `--port`) and env vars (`MCP_TRANSPORT`, `PORT`) to determine stdio vs HTTP/SSE mode
3. **Generate** an optimized Dockerfile with the MCP server package pre-installed (includes `EXPOSE` for HTTP servers)
4. **Build** the container image (cached with deterministic tags — only rebuilds when the command changes)
5. **Auto-configure** for detected transport:
   - **Stdio** → network=none, bidirectional stdin/stdout proxy
   - **HTTP/SSE** → network=bridge, port mapping, `--host 0.0.0.0` injected, container runs as daemon with stderr forwarded
6. **Launch** the container with minimal permissions:
   - Only your working directory mounted at `/work`
   - Running as your UID:GID
7. **Forward** signals (Ctrl+C → SIGTERM) for graceful shutdown

### Transport Auto-Detection

domcp scans for HTTP transport indicators in this priority order:

| Signal | Example | Result |
|--------|---------|--------|
| `--transport` flag | `--transport sse` | HTTP on default port (8000) |
| `--port` flag | `--port 3000` | HTTP on port 3000 |
| Combined flags | `--transport=sse --port=9090` | HTTP on port 9090 |
| `MCP_TRANSPORT` env | `--env MCP_TRANSPORT=streamable-http` | HTTP on default port |
| `PORT`/`MCP_PORT` env | `--env PORT=7777` | HTTP on port 7777 |
| None of the above | `uvx mcp-server-fetch` | Stdio (default) |

When HTTP mode is detected, domcp automatically:
- Upgrades `--network=none` to `--network=bridge`
- Adds `-p PORT:PORT` mapping
- Injects `--host 0.0.0.0` so the server is reachable outside the container
- Adds `EXPOSE PORT` to the Dockerfile
- Prints the connection URL: `http://localhost:PORT`

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
