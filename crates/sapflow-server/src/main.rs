//! sapflow: SRT in, Low-Latency HLS out, with the v4v stream-health callback.

mod callback;
mod config;
mod fdlimit;
mod http;
mod http1;
#[cfg(test)]
mod http_backend_tests;
mod own_http;
mod publish;
mod publish_control;
mod publish_observer;
mod publish_session_store;
mod publish_session_wire;
mod publish_sessions;
mod recorder;
mod rtmp;
mod srt;
mod srt_own;
mod stream;

use std::sync::Arc;

use callback::Callbacks;
use config::{Config, ConfigError};
use stream::Registry;

impl App {
    /// Logs a refused publish, rate-limited.
    pub fn log_refusal(&self, protocol: &str, reason: &dyn std::fmt::Debug) {
        let allowed = self
            .refusal_log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allow(std::time::Instant::now());
        if let Some(suppressed) = allowed {
            tracing::info!(protocol, ?reason, suppressed, "publish refused");
        }
    }
}

pub struct App {
    pub config: Config,
    pub publish_sessions: Option<publish_sessions::PublishSessions>,
    /// Failed publish attempts per address.
    pub auth_throttle: std::sync::Mutex<sapflow_core::limits::AuthThrottle>,
    /// Refusal log lines, at most 20 a minute.
    pub refusal_log: std::sync::Mutex<sapflow_core::limits::LogLimiter>,
    pub streams: Arc<Registry>,
    pub callbacks: Callbacks,
}

fn main() -> std::process::ExitCode {
    let config = match Config::load(|k| std::env::var(k).ok(), std::env::args().skip(1)) {
        Ok(c) => c,
        Err(ConfigError::Help) => {
            print!("{}", config::HELP);
            return std::process::ExitCode::SUCCESS;
        }
        Err(ConfigError::Version) => {
            println!("sapflow {}", env!("CARGO_PKG_VERSION"));
            return std::process::ExitCode::SUCCESS;
        }
        Err(ConfigError::Invalid(msg)) => {
            eprintln!("sapflow: {msg}\nRun sapflow --help for the settings.");
            return std::process::ExitCode::from(2);
        }
    };

    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_directives(
            std::env::var("RUST_LOG").ok().as_deref(),
        )))
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sapflow: cannot start runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(config)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "sapflow stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(config: Config) -> std::io::Result<()> {
    let (before, after) = fdlimit::raise();
    if after < 4_096 {
        tracing::warn!(
            before,
            after,
            "open-file limit is low; viewers past it will be refused"
        );
    } else {
        tracing::info!(before, after, "open-file limit");
    }
    if config.publisher.is_none() && config.control.is_none() {
        tracing::warn!("SAPFLOW_OPEN_PUBLISH=1: anyone can publish to any stage path");
    }
    if config.callback_url.is_none() {
        tracing::warn!("no SAPFLOW_CALLBACK_URL: stream events are only logged");
    }
    let publish_sessions = match &config.control {
        Some(control) => Some(
            publish_sessions::PublishSessions::open(control, config.callback_url.is_some())
                .await
                .map_err(|_| {
                    std::io::Error::other("cannot open scoped publish-session authority")
                })?,
        ),
        None => None,
    };
    let app = Arc::new(App {
        publish_sessions,
        callbacks: Callbacks::start(config.callback_url.clone(), config.callback_secret.clone()),
        streams: Arc::new(Registry::default()),
        refusal_log: std::sync::Mutex::new(sapflow_core::limits::LogLimiter::new(
            20,
            std::time::Duration::from_secs(60),
        )),
        auth_throttle: std::sync::Mutex::new(sapflow_core::limits::AuthThrottle::new(
            10,
            std::time::Duration::from_secs(60),
        )),
        config,
    });

    // SAPFLOW_HTTP_IMPL=own serves viewers from our own event loop.
    let http = match app.config.own_http_threads {
        Some(threads) => {
            let l = std::net::TcpListener::bind(app.config.http_addr)?;
            own_http::spawn(app.clone(), l, threads, own_http::Timeouts::default())?;
            tracing::info!(addr = %app.config.http_addr, threads, "HLS listening (own event loop)");
            None
        }
        None => {
            let l = tokio::net::TcpListener::bind(app.config.http_addr).await?;
            tracing::info!(addr = %app.config.http_addr, "HLS listening");
            Some(l)
        }
    };
    let srt_task = tokio::spawn(srt::listen(app.clone(), app.config.srt_addr));
    let rtmp_task = tokio::spawn({
        let app = app.clone();
        async move {
            match app.config.rtmp_addr {
                Some(addr) => rtmp::listen(app.clone(), addr).await,
                None => std::future::pending().await,
            }
        }
    });
    let control_task = tokio::spawn({
        let app = app.clone();
        async move {
            if let Some(control) = &app.config.control {
                let listener = tokio::net::TcpListener::bind(control.addr).await?;
                tracing::info!(addr=%control.addr,"private scoped control listening");
                publish_control::serve(listener, app).await
            } else {
                std::future::pending().await
            }
        }
    });
    let expiry_task = tokio::spawn({
        let app = app.clone();
        async move {
            let Some(sessions) = &app.publish_sessions else {
                return std::future::pending::<std::io::Result<()>>().await;
            };
            sessions
                .run_expiry()
                .await
                .map_err(|_| std::io::Error::other("scoped authority failed closed"))
        }
    });
    if let Some(dir) = &app.config.recordings {
        tracing::info!(dir = %dir.display(), "recording shows");
        tokio::spawn(recorder::expire_forever(
            dir.clone(),
            app.config.record_delete_after,
        ));
    }
    let http_task: std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>>>> =
        match http {
            Some(http) => Box::pin(http::serve(http, http::router(app.clone()), stop_signal())),
            None => Box::pin(async {
                stop_signal().await;
                Ok(())
            }),
        };

    tokio::select! {
        r = http_task => r,
        r = srt_task => match r {
            Ok(r) => r,
            Err(e) => Err(std::io::Error::other(e)),
        },
        r = rtmp_task => match r {
            Ok(r) => r,
            Err(e) => Err(std::io::Error::other(e)),
        },
        r = control_task => r.map_err(std::io::Error::other)?,
        r = expiry_task => r.map_err(std::io::Error::other)?,
    }
}

/// The log filter: `RUST_LOG` if set, else `info`. h264-reader logs an error
/// for every byte of a damaged or mid-frame Annex B stream and then carries
/// on; a publisher that starts mid-frame produced thousands of lines a
/// second. That source is off unless `RUST_LOG` names it.
fn log_directives(rust_log: Option<&str>) -> String {
    let base = match rust_log.map(str::trim) {
        Some(v) if !v.is_empty() => v,
        _ => "info",
    };
    if base.contains("h264_reader") {
        base.to_string()
    } else {
        format!("{base},h264_reader::annexb=off")
    }
}

/// Ctrl-C, or SIGTERM from `docker stop` and systemd. As a container's first
/// process, an unhandled SIGTERM is ignored and the stop waits for a kill.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod log_tests {
    use super::log_directives;

    #[test]
    fn h264_reader_noise_is_off_by_default() {
        assert_eq!(log_directives(None), "info,h264_reader::annexb=off");
        assert_eq!(log_directives(Some("  ")), "info,h264_reader::annexb=off");
        assert_eq!(log_directives(Some("warn")), "warn,h264_reader::annexb=off");
    }

    #[test]
    fn naming_h264_reader_in_rust_log_keeps_it() {
        assert_eq!(
            log_directives(Some("debug,h264_reader=error")),
            "debug,h264_reader=error"
        );
    }
}
