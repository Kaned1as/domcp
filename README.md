# domcp

domcp wraps MCP server commands (`uvx`, `npx`, `pipx`) in a container,
proxying stdio or exposing HTTP ports transparently. Only the current
working directory is mounted into the container by default.

## Synopsis

    domcp [OPTIONS] -- <COMMAND>...

## Description

domcp generates a Dockerfile for the given command, builds a container
image, and runs it with restricted permissions. The MCP transport mode
(stdio or HTTP/SSE) is detected automatically from command flags and
environment variables.

The container engine is auto-detected, preferring podman over docker.

## Options

    -e, --engine <ENGINE>        Container engine (default: auto-detect)
    -w, --workdir <DIR>          Working directory to mount (default: $PWD)
        --no-workdir             Don't mount any working directory
    -v, --extra-mount <PATH>     Additional directory to mount (repeatable)
        --env <KEY=VALUE>        Extra environment variable (repeatable)
        --no-env                 Don't pass host environment into container
        --network <MODE>         Network mode (default: engine default)
    -p, --port <HOST:CONTAINER>  Port mapping (repeatable; auto-added for HTTP)
        --rebuild                Force image rebuild
        --no-user-map            Don't map host UID/GID into container
        --dry-run                Print generated Dockerfile and exit
    -h, --help                   Print help
    -V, --version                Print version

## Installation

    cargo install --path .

Requires podman(1) or docker(1).

## Examples

Stdio server:

    domcp -- uvx mcp-server-fetch
    domcp -- npx -y @modelcontextprotocol/server-filesystem .
    domcp -- pipx run mcp-server-time

HTTP/SSE server (transport and port detected automatically):

    domcp -- uvx mcp-server-sse --transport sse --port 8080
    domcp --env MCP_TRANSPORT=sse --env PORT=8080 -- uvx mcp-server-sse

Allow network access:

    domcp --network host -- uvx mcp-server-fetch

Mount additional directories:

    domcp -v /data -- uvx mcp-server-filesystem . /data

Use docker instead of podman:

    domcp -e docker -- npx -y @modelcontextprotocol/server-everything

Preview without building:

    domcp --dry-run -- uvx mcp-server-fetch

## Client Configuration

Example `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "fetch": {
      "command": "domcp",
      "args": ["--network", "host", "--", "uvx", "mcp-server-fetch"]
    },
    "filesystem": {
      "command": "domcp",
      "args": ["--", "npx", "-y",
        "@modelcontextprotocol/server-filesystem", "."]
    },
    "sse-server": {
      "url": "http://localhost:8080/sse",
      "command": "domcp",
      "args": ["--", "uvx", "mcp-server-sse",
        "--transport", "sse", "--port", "8080"]
    }
  }
}
```

## Transport Detection

domcp scans command arguments and `--env` values to choose between
stdio and HTTP mode. The first matching rule wins:

    --transport sse|streamable-http|http      HTTP (port from --port or 8080)
    --port <N>                                HTTP on port N
    --transport=<val> / --port=<N>            combined forms, same as above
    MCP_TRANSPORT=sse|http|streamable-http    HTTP via environment
    PORT|MCP_PORT|FASTMCP_PORT=<N>            HTTP on port N via environment
    (none of the above)                       stdio

In HTTP mode domcp automatically:

- upgrades `--network none` to `--network bridge` (if explicitly set)
- adds `-p PORT:PORT`
- appends `--host 0.0.0.0` to the server command
- adds `EXPOSE` to the Dockerfile

## Security

Default isolation:

- Only `$PWD` is mounted (at the same path inside the container)
- Use `--no-workdir` if the server needs no filesystem access
- Network uses engine default (override with `--network none` for full isolation)
- Runs as host UID:GID (podman `--userns=keep-id`, docker `--user`)
- No `--privileged`
- SELinux relabeling (`:Z`) on podman
- Images built locally from official base images

## License

MIT

