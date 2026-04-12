use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A terminal spinner that runs in a background thread.
/// Stops and clears when dropped.
pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with the given message.
    pub fn start(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let msg = message.to_string();

        let handle = thread::spawn(move || {
            let mut i = 0;
            while running_clone.load(Ordering::Relaxed) {
                let frame = FRAMES[i % FRAMES.len()];
                eprint!("\r{} {} ", frame, msg);
                let _ = std::io::stderr().flush();
                i += 1;
                thread::sleep(Duration::from_millis(80));
            }
            // Clear the spinner line
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        });

        Spinner {
            running,
            handle: Some(handle),
        }
    }

    /// Stop the spinner and clear the line.
    pub fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
