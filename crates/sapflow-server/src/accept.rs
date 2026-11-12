//! Taking connections from a TCP listener without ending on an accept
//! error, for every listener the server runs on tokio: RTMP, the scoped
//! control API, WHIP signalling and ICE-TCP.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

/// How long to wait after a failed accept before trying again. Running out
/// of file descriptors clears as connections close, and a loop that did not
/// wait would spin meanwhile.
pub(crate) const RETRY: Duration = Duration::from_millis(100);

/// Where connections come from: a [`TcpListener`], or a test's stand-in.
pub(crate) trait Accept: Send + Sync {
    fn accept(&self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send;
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

impl Accept for TcpListener {
    fn accept(&self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send {
        TcpListener::accept(self)
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        TcpListener::local_addr(self)
    }
}

/// Failed accepts are logged at most this often per listener, with how
/// many were not logged since: running out of file descriptors can last,
/// and the listener retries every [`RETRY`].
pub(crate) const WARN_EVERY: Duration = Duration::from_secs(1);

/// When a listener last logged a failed accept, and how many it has not
/// logged since.
#[derive(Debug, Default)]
pub(crate) struct Warnings {
    last: Option<std::time::Instant>,
    quiet: u64,
}

impl Warnings {
    /// A failed accept at `now`: `Some(n)` to log it, `n` being how many
    /// went unlogged before it; `None` to leave it out.
    fn note(&mut self, now: std::time::Instant) -> Option<u64> {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < WARN_EVERY)
        {
            self.quiet += 1;
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.quiet))
    }
}

/// The next connection. An accept error (out of file descriptors, or a
/// connection reset before it was taken) is logged, at most once each
/// [`WARN_EVERY`], and retried after [`RETRY`]: it is never the end of the
/// listener, nor of the server.
pub(crate) async fn next(
    listener: &impl Accept,
    what: &'static str,
    warnings: &mut Warnings,
) -> (TcpStream, SocketAddr) {
    loop {
        match listener.accept().await {
            Ok(accepted) => return accepted,
            Err(error) => {
                if let Some(suppressed) = warnings.note(std::time::Instant::now()) {
                    tracing::warn!(%error, listener = what, suppressed, "accept failed");
                }
                tokio::time::sleep(RETRY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure that lasts is logged once a second, with the count left
    /// out; one after a quiet second is logged at once.
    #[test]
    fn failed_accepts_are_logged_once_a_second() {
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = Warnings::default();
        let logged: Vec<(u64, Option<u64>)> =
            (0..25u64).map(|i| (i * 100, w.note(at(i * 100)))).collect();
        let kept: Vec<_> = logged.iter().filter(|(_, n)| n.is_some()).collect();
        assert_eq!(kept, [&(0, Some(0)), &(1000, Some(9)), &(2000, Some(9))]);
        assert_eq!(w.note(at(4000)), Some(4), "after a quiet spell, at once");
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A listener whose first `failures` accepts fail, as they do when the
    /// process is out of file descriptors.
    pub(crate) struct Flaky {
        inner: TcpListener,
        failures: AtomicUsize,
    }

    impl Flaky {
        pub(crate) fn new(inner: TcpListener, failures: usize) -> Self {
            Self {
                inner,
                failures: AtomicUsize::new(failures),
            }
        }
    }

    impl Accept for Flaky {
        async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
            let failing = self
                .failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok();
            if failing {
                return Err(std::io::Error::other("injected accept failure"));
            }
            self.inner.accept().await
        }

        fn local_addr(&self) -> std::io::Result<SocketAddr> {
            self.inner.local_addr()
        }
    }
}
