use anyhow::{Context, Result};
use log::{debug, warn};
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;
use tokio::task::JoinHandle;

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
    pub async fn run(mut self) -> Result<i32> {
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

        let stdin_task = tokio::spawn(async move {
            let mut host_stdin = io::stdin();
            proxy_stream("stdin→container", &mut host_stdin, &mut child_stdin).await;
        });

        let stdout_task = tokio::spawn(async move {
            let mut host_stdout = io::stdout();
            proxy_stream("container→stdout", &mut child_stdout, &mut host_stdout).await;
        });

        let stderr_task = tokio::spawn(async move {
            let mut host_stderr = io::stderr();
            proxy_stream("container→stderr", &mut child_stderr, &mut host_stderr).await;
        });

        let status = self
            .child
            .wait()
            .await
            .context("Failed to wait for container process")?;

        debug!("Container exited with status: {}", status);

        // Stop waiting on host stdin once the child is gone. The output tasks
        // are still awaited so any buffered container output is drained.
        stdin_task.abort();

        await_task("stdin→container", stdin_task).await;
        await_task("container→stdout", stdout_task).await;
        await_task("container→stderr", stderr_task).await;

        Ok(status.code().unwrap_or(1))
    }
}

/// Copy data from reader to writer until EOF or error.
async fn proxy_stream<R, W>(label: &str, reader: &mut R, writer: &mut W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                debug!("{label}: EOF");
                break;
            }
            Ok(n) => {
                if let Err(e) = writer.write_all(&buf[..n]).await {
                    if e.kind() != io::ErrorKind::BrokenPipe {
                        warn!("{label}: write error: {e}");
                    }
                    break;
                }
                if let Err(e) = writer.flush().await {
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

    if let Err(e) = writer.shutdown().await {
        if e.kind() != io::ErrorKind::BrokenPipe {
            warn!("{label}: shutdown error: {e}");
        }
    }
}

async fn await_task(label: &str, task: JoinHandle<()>) {
    match task.await {
        Ok(()) => {}
        Err(e) if e.is_cancelled() => {
            debug!("{label}: task cancelled");
        }
        Err(e) => {
            warn!("{label}: task join error: {e}");
        }
    }
}
