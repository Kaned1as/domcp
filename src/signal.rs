use log::debug;
use tokio::sync::mpsc;

/// Create a receiver that yields a shutdown reason when the host process is
/// interrupted.
///
/// The task that owns the container `Child` should listen on this receiver and
/// terminate the child directly. That keeps child ownership local and avoids
/// platform-specific PID signaling.
pub fn shutdown_channel() -> mpsc::UnboundedReceiver<&'static str> {
    let (tx, rx) = mpsc::unbounded_channel();

    let ctrlc_tx = tx.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                let _ = ctrlc_tx.send("Ctrl+C");
            }
            Err(e) => debug!("Failed to listen for Ctrl+C: {}", e),
        }
    });

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
                let _ = tx.send("SIGTERM");
            }
            Err(e) => debug!("Failed to listen for SIGTERM: {}", e),
        }
    });

    rx
}
