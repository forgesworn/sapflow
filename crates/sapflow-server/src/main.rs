//! sapflow: SRT in, Low-Latency HLS out, with the v4v stream-health callback.

mod callback;
mod config;
mod fdlimit;
mod http;
mod publish;
mod recorder;
mod rtmp;
mod srt;
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
    /// Failed publish attempts per address.
    pub auth_throttle: std::sync::Mutex<sapflow_core::limits::AuthThrottle>,
    /// Refusal log lines, at most 20 a minute.
    pub refusal_log: std::sync::Mutex<sapflow_core::limits::LogLimiter>,
    pub streams: Registry,
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,srt_protocol=error,srt_tokio=error")
            }),
        )
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
    if config.publisher.is_none() {
        tracing::warn!("SAPFLOW_OPEN_PUBLISH=1: anyone can publish to any stage path");
    }
    if config.callback_url.is_none() {
        tracing::warn!("no SAPFLOW_CALLBACK_URL: stream events are only logged");
    }
    let app = Arc::new(App {
        callbacks: Callbacks::start(config.callback_url.clone(), config.callback_secret.clone()),
        streams: Registry::default(),
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

    let http = tokio::net::TcpListener::bind(app.config.http_addr).await?;
    tracing::info!(addr = %app.config.http_addr, "HLS listening");
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
    if let Some(dir) = &app.config.recordings {
        tracing::info!(dir = %dir.display(), "recording shows");
        tokio::spawn(recorder::expire_forever(
            dir.clone(),
            app.config.record_delete_after,
        ));
    }
    let http_task = http::serve(http, http::router(app.clone()), async {
        let _ = tokio::signal::ctrl_c().await;
    });

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
    }
}
