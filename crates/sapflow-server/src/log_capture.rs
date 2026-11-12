//! Test builds only: capturing what this thread logs.
//!
//! One process-wide subscriber, installed the first time a capture
//! starts, writes an event only for a thread that holds a [`Capture`]. A
//! subscriber of this thread's own (`tracing::subscriber::set_default`) is
//! not enough while the server's tests run in parallel: while it is the
//! only dispatcher, tracing-core 0.1 computes a callsite's interest from
//! the default of whichever thread reaches the callsite first, and caches
//! it. A test on another thread that reaches a callsite first, with no
//! subscriber of its own, caches "never" there, and this thread's events
//! at that callsite are then dropped. The global subscriber gives every
//! callsite "sometimes", so each event asks whether its thread captures.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

type Buffer = Arc<Mutex<Vec<u8>>>;

thread_local! {
    /// The buffer this thread's events go to, while a capture is held.
    static CAPTURE: RefCell<Option<Buffer>> = const { RefCell::new(None) };
}

/// What this thread logs, at every level, while it is held.
pub(crate) struct Capture {
    buf: Buffer,
    /// Not `Send`: the capture belongs to the thread that started it.
    _thread: std::marker::PhantomData<*const ()>,
}

/// Appends to this thread's capture, if it has one.
struct Writer;

impl std::io::Write for Writer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        CAPTURE.with(|c| {
            if let Some(buf) = c.borrow().as_ref() {
                buf.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(b);
            }
        });
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

/// Installs the process-wide subscriber, once.
fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(|| Writer)
            .with_filter(tracing_subscriber::filter::dynamic_filter_fn(|_, _| {
                capturing()
            }));
        let subscriber = tracing_subscriber::registry().with(layer);
        // Only this module sets a global subscriber in the server's tests.
        let _ = tracing::subscriber::set_global_default(subscriber);
        // Interest cached before the subscriber was installed is recomputed.
        tracing::callsite::rebuild_interest_cache();
    });
}

impl Capture {
    /// Starts capturing everything this thread logs.
    pub(crate) fn start() -> Self {
        install();
        let buf = Buffer::default();
        CAPTURE.with(|c| *c.borrow_mut() = Some(buf.clone()));
        Self {
            buf,
            _thread: std::marker::PhantomData,
        }
    }

    /// What has been captured so far.
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURE.with(|c| c.borrow_mut().take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A callsite that no other test reaches.
    fn probe(n: u32) {
        tracing::debug!(n, "log capture probe");
    }

    /// Another thread reaching a callsite first, with nothing capturing
    /// there, does not hide this thread's events at that callsite.
    #[test]
    fn a_callsite_another_thread_reached_first_is_still_captured() {
        let capture = Capture::start();
        std::thread::spawn(|| probe(1))
            .join()
            .expect("the other thread");
        probe(2);
        let logs = capture.text();
        assert!(
            logs.contains("log capture probe") && logs.contains("n=2"),
            "captured: {logs:?}"
        );
        assert!(!logs.contains("n=1"), "only this thread: {logs:?}");
    }
}
