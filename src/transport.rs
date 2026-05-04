use log::{debug, info};

/// The detected MCP transport mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// JSON-RPC over stdin/stdout (the default for most MCP servers).
    Stdio,
    /// HTTP-based: SSE or Streamable HTTP on a specific port.
    Http {
        port: u16,
    },
}

/// Well-known flag names that carry a port number as their next argument.
const PORT_FLAGS: &[&str] = &["--port", "-p"];

/// Well-known flag names that carry a host/bind address as their next argument.
const HOST_FLAGS: &[&str] = &["--host", "--bind", "--listen"];

/// Well-known transport flag values that indicate HTTP mode.
const HTTP_TRANSPORT_VALUES: &[&str] = &["sse", "streamable-http", "http"];

/// Default port that MCP HTTP servers tend to use when not specified explicitly.
const DEFAULT_HTTP_PORT: u16 = 8000;

/// Detect the MCP transport mode from the command arguments and environment
/// variables that will be passed into the container.
///
/// The heuristics (in priority order):
///
/// 1. Explicit `--transport sse|streamable-http|http` flag → HTTP
/// 2. `--port <N>` / `-p <N>` flag → HTTP on that port
/// 3. Environment variable `MCP_TRANSPORT=sse|http|streamable-http` → HTTP
/// 4. Environment variable with a port hint (e.g. `FASTMCP_PORT`, `PORT`) → HTTP
/// 5. Everything else → Stdio
pub fn detect(command: &[String], envs: &[String]) -> Transport {
    // --- pass 1: scan flags for explicit transport declaration ------------------
    if let Some(t) = scan_transport_flag(command) {
        let port = scan_port_flag(command).unwrap_or(DEFAULT_HTTP_PORT);
        info!("Detected HTTP transport (--transport {t}) on port {port}");
        return Transport::Http { port };
    }

    // --- pass 2: scan for a port flag (implies HTTP) ---------------------------
    if let Some(port) = scan_port_flag(command) {
        info!("Detected HTTP transport (port flag) on port {port}");
        return Transport::Http { port };
    }

    // --- pass 3: scan =style combined flags (--port=8080, --transport=sse) -----
    if let Some(t) = scan_combined_transport_flag(command) {
        let port = scan_combined_port_flag(command).unwrap_or(DEFAULT_HTTP_PORT);
        info!("Detected HTTP transport (--transport={t}) on port {port}");
        return Transport::Http { port };
    }

    if let Some(port) = scan_combined_port_flag(command) {
        info!("Detected HTTP transport (--port={port})");
        return Transport::Http { port };
    }

    // --- pass 4: scan env vars -------------------------------------------------
    if let Some(transport) = scan_env_transport(envs) {
        let port = scan_env_port(envs).unwrap_or(DEFAULT_HTTP_PORT);
        info!("Detected HTTP transport (env MCP_TRANSPORT={transport}) on port {port}");
        return Transport::Http { port };
    }

    if let Some(port) = scan_env_port(envs) {
        info!("Detected HTTP transport (env port hint) on port {port}");
        return Transport::Http { port };
    }

    debug!("No HTTP transport indicators found, defaulting to stdio");
    Transport::Stdio
}

// ---------------------------------------------------------------------------
// Argument scanners
// ---------------------------------------------------------------------------

/// Look for `--transport <value>` where value indicates HTTP.
fn scan_transport_flag(command: &[String]) -> Option<String> {
    let mut iter = command.iter();
    while let Some(arg) = iter.next() {
        if arg == "--transport" || arg == "-t" {
            if let Some(val) = iter.next() {
                let lower = val.to_lowercase();
                if HTTP_TRANSPORT_VALUES.contains(&lower.as_str()) {
                    return Some(lower);
                }
            }
        }
    }
    None
}

/// Look for `--transport=<value>` combined form.
fn scan_combined_transport_flag(command: &[String]) -> Option<String> {
    for arg in command {
        if let Some(rest) = arg.strip_prefix("--transport=") {
            let lower = rest.to_lowercase();
            if HTTP_TRANSPORT_VALUES.contains(&lower.as_str()) {
                return Some(lower);
            }
        }
    }
    None
}

/// Look for `--port <N>` / `-p <N>` and return the port number.
///
/// Note: we only look at arguments *after* the runner name (index 0) and only
/// when the flag is NOT already consumed by domcp's own `-p` (which uses
/// `HOST:CONTAINER` format — contains a colon).
fn scan_port_flag(command: &[String]) -> Option<u16> {
    let mut iter = command.iter();
    while let Some(arg) = iter.next() {
        if PORT_FLAGS.contains(&arg.as_str()) {
            if let Some(val) = iter.next() {
                // Ignore domcp-style "HOST:CONTAINER" values
                if val.contains(':') {
                    continue;
                }
                if let Ok(port) = val.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Look for `--port=<N>` combined form.
fn scan_combined_port_flag(command: &[String]) -> Option<u16> {
    for arg in command {
        for prefix in &["--port=", "-p="] {
            if let Some(rest) = arg.strip_prefix(prefix) {
                if !rest.contains(':') {
                    if let Ok(port) = rest.parse::<u16>() {
                        return Some(port);
                    }
                }
            }
        }
    }
    None
}

/// Scan environment variable definitions for transport hints.
fn scan_env_transport(envs: &[String]) -> Option<String> {
    for env in envs {
        if let Some((key, val)) = env.split_once('=') {
            let key_upper = key.to_uppercase();
            if key_upper == "MCP_TRANSPORT"
                || key_upper == "TRANSPORT"
                || key_upper == "FASTMCP_TRANSPORT"
            {
                let lower = val.to_lowercase();
                if HTTP_TRANSPORT_VALUES.contains(&lower.as_str()) {
                    return Some(lower);
                }
            }
        }
    }
    None
}

/// Scan environment variables for port hints.
fn scan_env_port(envs: &[String]) -> Option<u16> {
    for env in envs {
        if let Some((key, val)) = env.split_once('=') {
            let key_upper = key.to_uppercase();
            if key_upper == "PORT"
                || key_upper == "MCP_PORT"
                || key_upper == "FASTMCP_PORT"
                || key_upper == "SERVER_PORT"
            {
                if let Ok(port) = val.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Rewrite the command to ensure the server binds on 0.0.0.0 inside the
/// container (required for port-forwarding to work from outside).
///
/// If the server already has a `--host` / `--bind` flag we leave it alone.
/// Otherwise we append `--host 0.0.0.0`.
pub fn ensure_bind_all(command: &mut Vec<String>) {
    // Check if there's already a host/bind flag
    let has_host = command.iter().any(|a| {
        HOST_FLAGS.contains(&a.as_str())
            || HOST_FLAGS
                .iter()
                .any(|f| a.starts_with(&format!("{f}=")))
    });

    if !has_host {
        info!("Appending --host 0.0.0.0 so the server is reachable outside the container");
        command.push("--host".to_string());
        command.push("0.0.0.0".to_string());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect() ----------------------------------------------------------

    #[test]
    fn test_stdio_default() {
        let cmd = vec!["uvx".into(), "mcp-server-fetch".into()];
        assert_eq!(detect(&cmd, &[]), Transport::Stdio);
    }

    #[test]
    fn test_detect_transport_flag_sse() {
        let cmd = vec![
            "uvx".into(),
            "mcp-server-sse".into(),
            "--transport".into(),
            "sse".into(),
        ];
        assert_eq!(detect(&cmd, &[]), Transport::Http { port: DEFAULT_HTTP_PORT });
    }

    #[test]
    fn test_detect_transport_flag_streamable_http() {
        let cmd = vec![
            "uvx".into(),
            "mcp-server-x".into(),
            "--transport".into(),
            "streamable-http".into(),
            "--port".into(),
            "3000".into(),
        ];
        assert_eq!(detect(&cmd, &[]), Transport::Http { port: 3000 });
    }

    #[test]
    fn test_detect_combined_transport_flag() {
        let cmd = vec![
            "npx".into(),
            "-y".into(),
            "some-mcp-server".into(),
            "--transport=sse".into(),
            "--port=9090".into(),
        ];
        assert_eq!(detect(&cmd, &[]), Transport::Http { port: 9090 });
    }

    #[test]
    fn test_detect_port_flag_implies_http() {
        let cmd = vec![
            "uvx".into(),
            "mcp-server-http".into(),
            "--port".into(),
            "8080".into(),
        ];
        assert_eq!(detect(&cmd, &[]), Transport::Http { port: 8080 });
    }

    #[test]
    fn test_detect_combined_port_flag() {
        let cmd = vec![
            "uvx".into(),
            "mcp-server-http".into(),
            "--port=4000".into(),
        ];
        assert_eq!(detect(&cmd, &[]), Transport::Http { port: 4000 });
    }

    #[test]
    fn test_detect_env_transport() {
        let cmd = vec!["uvx".into(), "mcp-server-x".into()];
        let envs = vec!["MCP_TRANSPORT=sse".into()];
        assert_eq!(detect(&cmd, &envs), Transport::Http { port: DEFAULT_HTTP_PORT });
    }

    #[test]
    fn test_detect_env_port() {
        let cmd = vec!["uvx".into(), "mcp-server-x".into()];
        let envs = vec!["PORT=7777".into()];
        assert_eq!(detect(&cmd, &envs), Transport::Http { port: 7777 });
    }

    #[test]
    fn test_detect_env_transport_and_port() {
        let cmd = vec!["uvx".into(), "mcp-server-x".into()];
        let envs = vec![
            "MCP_TRANSPORT=streamable-http".into(),
            "MCP_PORT=5555".into(),
        ];
        assert_eq!(detect(&cmd, &envs), Transport::Http { port: 5555 });
    }

    #[test]
    fn test_detect_ignores_domcp_port_format() {
        // domcp's own -p uses HOST:CONTAINER, which should not confuse detection
        let cmd = vec![
            "uvx".into(),
            "mcp-server-fetch".into(),
        ];
        // This only appears in Args.ports, not in the command itself
        assert_eq!(detect(&cmd, &[]), Transport::Stdio);
    }

    #[test]
    fn test_detect_port_flag_ignores_colon_format() {
        let cmd = vec![
            "uvx".into(),
            "mcp-server-fetch".into(),
            "--port".into(),
            "8080:8080".into(),
        ];
        // Contains a colon → not a simple port, ignored by scan_port_flag
        assert_eq!(detect(&cmd, &[]), Transport::Stdio);
    }

    // --- ensure_bind_all() -------------------------------------------------

    #[test]
    fn test_ensure_bind_all_appends() {
        let mut cmd = vec!["uvx".into(), "mcp-server-sse".into()];
        ensure_bind_all(&mut cmd);
        assert_eq!(cmd, vec!["uvx", "mcp-server-sse", "--host", "0.0.0.0"]);
    }

    #[test]
    fn test_ensure_bind_all_skips_when_present() {
        let mut cmd = vec![
            "uvx".into(),
            "mcp-server-sse".into(),
            "--host".into(),
            "127.0.0.1".into(),
        ];
        ensure_bind_all(&mut cmd);
        // Should NOT double-add
        assert_eq!(
            cmd,
            vec!["uvx", "mcp-server-sse", "--host", "127.0.0.1"]
        );
    }

    #[test]
    fn test_ensure_bind_all_skips_combined() {
        let mut cmd = vec![
            "uvx".into(),
            "mcp-server-sse".into(),
            "--host=0.0.0.0".into(),
        ];
        ensure_bind_all(&mut cmd);
        assert_eq!(cmd, vec!["uvx", "mcp-server-sse", "--host=0.0.0.0"]);
    }
}
