domcp
=====

Ever felt scared that AI will rummage through your files and find
and use API keys or admin accounts you had no desire of allowing to touch?

Fear not. `domcp` is a **Docker-for-MCPs**. 

It wraps MCP server commands (`uvx`, `npx`, `pipx`) in a container,
proxying stdio or exposing HTTP ports transparently. Only the working
directory is mounted into the container by default, nothing else! Host
environment variables are not forwarded unless you opt in with
`-e/--expose-env` patterns.

Synopsis
--------

```bash
domcp [OPTIONS] -- <COMMAND>...
```

Description
-----------

`domcp` generates a Dockerfile for the given command, builds a container
image, and runs it with restricted permissions. The docker image is cached
for subsequent invocations.

The MCP transport mode (stdio or HTTP/SSE) is detected automatically
from command flags and any environment variables that domcp will pass into the container.

The container engine is auto-detected, supporting `podman` and `docker`.

Client Configuration
--------------------

Just prepend your MCP startup command with `domcp [options] --`.
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
      "args": ["--", "npx", "-y", "@modelcontextprotocol/server-filesystem", "."]
    },
    "sse-server": {
      "url": "http://localhost:8080/sse",
      "command": "domcp",
      "args": ["--", "uvx", "mcp-server-sse", "--transport", "sse", "--port", "8080"]
    }
  }
}
```

Installation
------------

You can grab the latest released binary from [Releases][releases] page or use permalinks below:

* [**aarch64 (arm64) Linux release binary**][linux-aarch64]
* [**armeabi-v7a (armv7) Linux release binary**][linux-armv7]
* [**riscv64gc (riscv64) Linux release binary**][linux-riscv64]
* [**x86_64 (amd64) Linux release binary**][linux-amd64]
* [**x86_64 (amd64) Windows executable**][windows-amd64]


Arch users can install AUR package [domcp-git][aur]

Development version can be installed by cloning this repo and executing:

```bash
cargo install --path .
```

Don't forget it requires podman or docker as an underlying container engine.

Options
-------

        --engine <ENGINE>        Container engine (default: auto-detect)
    -w, --workdir <DIR>          Working directory to mount (default: $PWD)
        --no-workdir             Don't mount any working directory
    -v, --extra-mount <PATH>     Additional directory to mount (repeatable)
    -e, --expose-env <PATTERN>   Expose host env vars matching PATTERN (repeatable; prefixes like `ATLASSIAN_*`)
        --network <MODE>         Network mode (default: engine default)
    -i, --install <PKG>          Extra package to install (repeatable)
    -p, --port <HOST:CONTAINER>  Port mapping (repeatable; auto-added for HTTP)
        --rebuild                Force image rebuild
        --no-user-map            Don't map host UID/GID into container
        --dry-run                Print generated Dockerfile and exit
    -h, --help                   Print help
    -V, --version                Print version

Examples
--------

Simple stdio-based server:

```bash
domcp -- uvx mcp-server-fetch
domcp -- npx -y @modelcontextprotocol/server-filesystem .
domcp -- pipx run mcp-server-time
```

Install extra tools needed by the wrapped MCP server:

```bash
domcp -i git -- uvx git-mcp-server
domcp -i openssh -- uvx slepp-ssh-mcp
```

HTTP/SSE server (transport and port detected automatically):

```bash
domcp -- uvx mcp-server-sse --transport sse --port 8080
MCP_TRANSPORT=sse PORT=8080 domcp -e MCP_TRANSPORT -e PORT -- uvx mcp-server-sse
```

Allow network access:

```bash
domcp --network host -- uvx mcp-server-fetch
```

Mount additional directories:

```bash
domcp -v /data -- uvx mcp-server-filesystem /data
domcp -v ~/.ssh -- uvx slepp-ssh-mcp
```

Use docker instead of podman:

```bash
domcp --engine docker -- npx -y @modelcontextprotocol/server-everything
```

Pass filtered env variables to the server:

```bash
domcp -e "ATLASSIAN_*" -- uvx atlassian-mcp-server
```

Preview without building:

```bash
domcp --dry-run -- uvx mcp-server-fetch
```

Real-world examples
-------------------

Here are the examples of MCP servers that I use daily:

AWS:
```bash
domcp -e "AWS_*" -i awscli -v ~/.aws --no-workdir -- uvx awslabs.aws-api-mcp-server@latest
```

SSH:
```bash
domcp -i openssh -v ~/.ssh -v ~/.dotfiles/ssh -- uvx --from slepp-ssh-mcp ssh-mcp
```

Jira/Confluence
```bash
domcp -e "JIRA_*" -e "CONFLUENCE_*" --no-workdir -- uvx mcp-atlassian
```

Slack
```bash
domcp -e "SLACK_MCP_*" --no-workdir -- npx -y slack-mcp-server --transport stdio
```

Transport Detection
-------------------

domcp scans command arguments and environment values exposed via `-e/--expose-env` to choose between
stdio and HTTP mode. The first matching rule wins:

    --transport sse|streamable-http|http      HTTP (port from --port or 8080)
    --port <N>                                HTTP on port N
    --transport=<val> / --port=<N>            combined forms, same as above
    MCP_TRANSPORT=sse|http|streamable-http    HTTP via environment
    PORT|MCP_PORT|FASTMCP_PORT=<N>            HTTP on port N via environment
    (none of the above)                       stdio

In HTTP mode domcp automatically:

- adds `-p PORT:PORT`
- adds `EXPOSE` to the Dockerfile

Security
--------

Default isolation:

- Only `$PWD` is mounted (at the same path inside the container)
- No host environment variables are forwarded unless explicitly exposed
- Use `--no-workdir` if the server needs no filesystem access
- Use `--network none` if the server needs no network access
- Runs as host UID:GID
- No `--privileged` flag
- Images built locally from official base images

Known issues
-------------

* On the first run `domcp` needs to generate a Dockerfile, pull the base image,
  let it install the runtime, then the MCP server inside the container, then run 
  the container. These steps can take a while on the first run, especially 
  if the network bandwidth is limited, so set the timeouts appropriately.

License
-------------

    Copyright (C) 2026  Oleg `Kanedias` Chernovskiy

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.


[aur]:           https://aur.archlinux.org/packages/domcp-git
[releases]:      https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest
[windows-amd64]: https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest/downloads/target/x86_64-pc-windows-msvc/release/domcp.exe
[linux-amd64]:   https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest/downloads/target/x86_64-unknown-linux-musl/release/domcp
[linux-armv7]:   https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest/downloads/target/armv7-unknown-linux-musleabihf/release/domcp
[linux-aarch64]: https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest/downloads/target/aarch64-unknown-linux-musl/release/domcp
[linux-riscv64]: https://gitlab.com/Kanedias/domcp/-/releases/permalink/latest/downloads/target/riscv64gc-unknown-linux-musl/release/domcp

