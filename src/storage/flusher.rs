use std::{
    sync::{
        Arc,
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use parking_lot::Mutex;

use crate::storage::buffer_pool::BufferPool;

// * TODO: make the flush duration configurable.

/// The interval at which background thread wakes up to flush dirty pages.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Manages the lifecycle of the background writing thread.
pub struct BackgroundFlusher {
    /// Channel sender used to signal the background thread to shut down.
    shutdown_tx: Option<Sender<()>>,

    /// The handle to the spawned thread, allowing us to safely block
    /// during shutdown until all final disk I/O completes.
    handle: Option<JoinHandle<()>>,
}

impl Drop for BackgroundFlusher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl BackgroundFlusher {
    /// Spawns a background thread that flushes dirty pages in the BufferPool
    /// at `FLUSH_INTERVAL`s.
    /// Takes an `Arc<Mutex<BufferPool>>` to share ownership with the
    /// foreground storage engine.
    pub fn start(pool: Arc<Mutex<BufferPool>>) -> Self {
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            loop {
                // Blocks the current thread for 100ms, or until a shutdown signal
                // is received.
                // If timeout is reached, we flush all the dirty pages to disk,
                // and log if an error does occur.
                match shutdown_rx.recv_timeout(FLUSH_INTERVAL) {
                    Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Shutdown signal received, exit the loop.
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let mut pool_guard = pool.lock()
                            && let Err(err) = pool_guard.flush_all_pages()
                        {
                            eprintln!("Background flusher encountered an error: {:?}", err);
                        }
                    }
                }
            }
            // Final flush to ensure durability; if we recieve a signal before
            // timeout.
            if let mut pool_guard = pool.lock()
                && let Err(err) = pool_guard.flush_all_pages()
            {
                eprintln!("Failed final background flush on shutdown: {:?}", err);
            }
        });
        Self {
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    /// Signals the background thread to stop and waits for it to finish its
    /// final flush.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Signal the thread to wake up immediately and exit.
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            // Block until the final flush is complete.
            let _ = handle.join();
        }
    }
}
