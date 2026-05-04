use log::debug;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::sync::atomic::{AtomicU32, Ordering};

/// Global storage for the container process PID so signal handlers can reach it.
static CHILD_PID: AtomicU32 = AtomicU32::new(0);

/// Register the child PID for signal forwarding and set up signal handlers.
///
/// When the user hits Ctrl+C (SIGINT) or the process receives SIGTERM,
/// we forward the signal to the container process so it shuts down cleanly.
pub fn setup_signal_forwarding(child_pid: u32) {
    CHILD_PID.store(child_pid, Ordering::SeqCst);

    // We use a simple approach: set up a ctrlc handler that forwards SIGTERM
    // to the child. This works because:
    // 1. The container runtime (podman/docker) forwards signals to the container PID 1
    // 2. SIGTERM is the standard graceful shutdown signal
    if let Err(e) = ctrlc::set_handler(move || {
        let pid = CHILD_PID.load(Ordering::SeqCst);
        if pid != 0 {
            debug!("Forwarding SIGTERM to child PID {}", pid);
            let _ = signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
    }) {
        debug!("Failed to set Ctrl+C handler: {}", e);
    }
}
