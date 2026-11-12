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

/// The next connection. An accept error (out of file descriptors, or a
/// connection reset before it was taken) is logged and retried after
/// [`RETRY`]: it is never the end of the listener, nor of the server.
pub(crate) async fn next(listener: &impl Accept, what: &'static str) -> (TcpStream, SocketAddr) {
    loop {
        match listener.accept().await {
            Ok(accepted) => return accepted,
            Err(error) => {
                tracing::warn!(%error, listener = what, "accept failed");
                tokio::time::sleep(RETRY).await;
            }
        }
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
