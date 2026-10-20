//! The stream-health callback to v4v: the only outbound call sapflow makes.
//!
//! Events go through one queue and are sent in order, so an `unpublish`
//! never overtakes the `publish` before it.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use sapflow_core::{StreamEvent, StreamPath, callback_url};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Callbacks {
    tx: Option<mpsc::UnboundedSender<(StreamPath, StreamEvent)>>,
}

impl Callbacks {
    /// Starts the sender. With no URL configured, events are only logged.
    pub fn start(url: Option<String>, secret: Option<String>) -> Self {
        let Some(url) = url else {
            return Self { tx: None };
        };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(url, secret, rx));
        Self { tx: Some(tx) }
    }

    pub fn send(&self, path: &StreamPath, event: StreamEvent) {
        tracing::info!(path = %path.as_path(), event = event.name(), "stream event");
        if let Some(tx) = &self.tx {
            let _ = tx.send((path.clone(), event));
        }
    }
}

async fn run(
    template: String,
    secret: Option<String>,
    mut rx: mpsc::UnboundedReceiver<(StreamPath, StreamEvent)>,
) {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    while let Some((path, event)) = rx.recv().await {
        let url = callback_url(&template, &path, secret.as_deref());
        let body = event.callback_body(&path);
        let mut delay = Duration::from_millis(500);
        for attempt in 1..=4 {
            let req = Request::builder()
                .method(Method::POST)
                .uri(&url)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.clone())));
            let Ok(req) = req else {
                tracing::error!(event = event.name(), "callback URL is not valid");
                break;
            };
            match tokio::time::timeout(Duration::from_secs(5), client.request(req)).await {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    tracing::debug!(event = event.name(), "callback delivered");
                    break;
                }
                Ok(Ok(resp)) if resp.status().is_client_error() => {
                    // v4v refused it (wrong token or path): retrying won't help.
                    tracing::warn!(event = event.name(), status = %resp.status(), "callback refused");
                    break;
                }
                Ok(Ok(resp)) => {
                    tracing::warn!(event = event.name(), status = %resp.status(), attempt, "callback failed");
                }
                Ok(Err(e)) => {
                    tracing::warn!(event = event.name(), error = %e, attempt, "callback failed");
                }
                Err(_) => tracing::warn!(event = event.name(), attempt, "callback timed out"),
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
    }
}
