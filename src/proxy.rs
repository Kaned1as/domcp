use anyhow::{Context, Result};
use log::{debug, warn};
use std::io::{self, Read, Write};
use std::process::Child;
use std::thread;

/// Bidirectional stdio proxy between the host process and the container.
///
/// MCP servers using stdio transport work by reading JSON-RPC messages from
/// stdin and writing responses to stdout. This proxy transparently forwards
/// both directions so the MCP client (e.g. Claude Desktop) sees the
/// containerized server as if it were running locally.
pub struct StdioProxy {
    child: Child,
}

impl StdioProxy {
    pub fn new(child: Child) -> Self {
        Self { child }
    }

    /// Run the bidirectional proxy until the child exits.
    ///
    /// Returns the child's exit status code.
    pub fn run(mut self) -> Result<i32> {
        let mut child_stdin = self
            .child
            .stdin
            .take()
            .context("Failed to take child stdin")?;
        let mut child_stdout = self
            .child
            .stdout
            .take()
            .context("Failed to take child stdout")?;
        let mut child_stderr = self
            .child
            .stderr
            .take()
            .context("Failed to take child stderr")?;

        // Spawn thread: host stdin → container stdin
        let stdin_thread = thread::Builder::new()
            .name("stdin-proxy".to_string())
            .spawn(move || {
                proxy_stream("stdin→container", &mut io::stdin().lock(), &mut child_stdin);
            })
            .context("Failed to spawn stdin proxy thread")?;

        // Spawn thread: container stdout → host stdout
        let stdout_thread = thread::Builder::new()
            .name("stdout-proxy".to_string())
            .spawn(move || {
                proxy_stream(
                    "container→stdout",
                    &mut child_stdout,
                    &mut io::stdout().lock(),
                );
            })
            .context("Failed to spawn stdout proxy thread")?;

        // Spawn thread: container stderr → host stderr
        let stderr_thread = thread::Builder::new()
            .name("stderr-proxy".to_string())
            .spawn(move || {
                proxy_stream(
                    "container→stderr",
                    &mut child_stderr,
                    &mut io::stderr().lock(),
                );
            })
            .context("Failed to spawn stderr proxy thread")?;

        // Wait for child to exit
        let status = self
            .child
            .wait()
            .context("Failed to wait for container process")?;

        debug!("Container exited with status: {}", status);

        // Wait for proxy threads (they'll finish once streams close)
        let _ = stdin_thread.join();
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();

        Ok(status.code().unwrap_or(1))
    }
}

/// Copy data from reader to writer until EOF or error.
fn proxy_stream(label: &str, reader: &mut dyn Read, writer: &mut dyn Write) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                debug!("{label}: EOF");
                break;
            }
            Ok(n) => {
                if let Err(e) = writer.write_all(&buf[..n]) {
                    // Broken pipe is expected when the other side closes
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        warn!("{label}: write error: {e}");
                    }
                    break;
                }
                if let Err(e) = writer.flush() {
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        warn!("{label}: flush error: {e}");
                    }
                    break;
                }
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::BrokenPipe {
                    warn!("{label}: read error: {e}");
                }
                break;
            }
        }
    }
}
