//! At start, the recordings the app never heard about: each outbox entry
//! ([`crate::recording_outbox`]) left by a run that ended before its
//! `recordSegmentComplete` was delivered.
//!
//! The entries are listed before any listener starts, so none that this
//! run's own recordings write is among them, and then sent from a background
//! task: publishers connect while it runs. Each goes through the callback
//! queue with its usual four attempts, one at a time and at least
//! [`INTERVAL`] apart (60 a minute at most, half of the 120 a minute v4v
//! takes from one address, leaving the rest to live stages), and its entry is
//! removed only once the app has answered 2xx. A refusal (4xx) leaves the
//! entry for the next start. A failure (the app down or answering 5xx, as
//! after a power cut that restarts it with sapflow) is tried again in later
//! rounds, the first [`RETRY_FIRST`] after, doubling to [`RETRY_MAX`], until
//! it is delivered, refused or expired, or sapflow stops; after
//! [`STOP_AFTER_FAILURES`] failures in a row a round ends, and the entries
//! not yet tried in it wait for the next round too.
//!
//! - A `finished` entry is sent as its live callback was, or would have
//!   been: the same state, reason, duration and, for a scoped recording,
//!   identity, with the sequence its recording reserved.
//! - An `open` entry, whose writer never finished, is sent `incomplete` with
//!   `server-interrupted` and the duration of the whole fragments in the
//!   file ([`sapflow_hls::fmp4::recorded_prefix`]). The file is never
//!   rewritten, truncated or copied: players stop at its partial last
//!   fragment, if it has one. A file with no whole fragment is not reported.
//! - An entry whose recording has gone, or is older than
//!   `SAPFLOW_RECORD_DELETE_AFTER`, is removed and not sent. One that cannot
//!   be read is left alone, with a warning, until expiry removes it.
//!
//! Recording files are written only by sapflow, in a directory only it can
//! write; they are still parsed as untrusted bytes, bounded, and a recording
//! is opened without following a symbolic link at its end.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use sapflow_core::{RecordingTracks, StreamPath};
use tokio::time::Instant;

use crate::callback::{Callbacks, Delivery, completion_body};
use crate::recorder::{IncompleteReason, duration_ms};
use crate::recording_outbox::{Outbox, OutboxEntry, ReadError};

/// The least time between two recovered callbacks.
pub const INTERVAL: Duration = Duration::from_secs(1);
/// Entries sent at one start, oldest first; the rest wait for the next.
pub const MAX_PER_START: usize = 4096;
/// Failed deliveries in a row after which a round ends.
pub const STOP_AFTER_FAILURES: usize = 3;
/// The wait before the first round that retries failed entries.
pub const RETRY_FIRST: Duration = Duration::from_secs(30);
/// The longest wait between retry rounds.
pub const RETRY_MAX: Duration = Duration::from_secs(600);

/// How fast recovery goes.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    /// The least time between two recovered callbacks.
    pub interval: Duration,
    /// The wait before the first retry round, doubled each round.
    pub retry_first: Duration,
    /// The longest wait between retry rounds.
    pub retry_max: Duration,
}

impl Pacing {
    pub const PRODUCTION: Self = Self {
        interval: INTERVAL,
        retry_first: RETRY_FIRST,
        retry_max: RETRY_MAX,
    };
}

/// The outbox's entries at start, oldest first.
pub struct Pending {
    names: Vec<OsString>,
}

impl Pending {
    /// Lists the outbox. Call it before any listener starts. Blocks.
    pub fn list(outbox: &Outbox, limit: usize) -> Self {
        let names = match outbox.list(limit) {
            Ok((names, more)) => {
                if !names.is_empty() {
                    tracing::info!(
                        pending = names.len(),
                        more,
                        "recordings not reported before the last stop; reporting them"
                    );
                }
                if more > 0 {
                    tracing::warn!(
                        more,
                        "more unreported recordings than one start reports; the rest at the next start"
                    );
                }
                names
            }
            Err(error) => {
                tracing::error!(%error, "cannot list the recording outbox; no recording is recovered");
                Vec::new()
            }
        };
        Self { names }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
}

/// What one start's recovery did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub delivered: usize,
    pub refused: usize,
    pub failed: usize,
    /// Entries removed unsent: the recording gone, expired or empty.
    pub dropped: usize,
    /// Entries left unsent: unreadable, or written by a newer sapflow.
    pub left: usize,
    /// Rounds that retried failed entries.
    pub retry_rounds: usize,
    /// Whether a round ended after [`STOP_AFTER_FAILURES`] failures.
    pub stopped: bool,
}

enum Prepared {
    Send {
        path: StreamPath,
        body: Bytes,
        entry: crate::recording_outbox::Entry,
        interrupted: bool,
    },
    Drop {
        file: String,
        why: &'static str,
    },
    Leave {
        why: &'static str,
    },
}

/// Sends what `pending` owes, through `callbacks`, at most one every
/// `pacing.interval`, retrying failures in later rounds. Returns once every
/// entry has been delivered, refused, removed or left; a stop aborts it.
pub async fn run(
    outbox: Arc<Outbox>,
    pending: Pending,
    callbacks: Callbacks,
    max_age: Duration,
    pacing: Pacing,
) -> Summary {
    let mut summary = Summary::default();
    let mut round: std::collections::VecDeque<OsString> = pending.names.into();
    let mut backoff = pacing.retry_first;
    while !round.is_empty() {
        let mut failed = Vec::new();
        let mut failures = 0;
        while let Some(name) = round.pop_front() {
            match send_one(&outbox, &name, &callbacks, max_age, pacing.interval).await {
                Sent::Not(counted) => counted.count(&mut summary),
                Sent::Delivered => {
                    summary.delivered += 1;
                    failures = 0;
                }
                Sent::Refused => {
                    summary.refused += 1;
                    failures = 0;
                }
                Sent::Failed => {
                    summary.failed += 1;
                    failed.push(name);
                    failures += 1;
                    if failures >= STOP_AFTER_FAILURES && !round.is_empty() {
                        tracing::warn!(
                            failures,
                            waiting = round.len(),
                            "recovered recordings not delivered; the rest wait for the next round"
                        );
                        summary.stopped = true;
                        failed.extend(round.drain(..));
                    }
                }
            }
        }
        if failed.is_empty() {
            break;
        }
        tracing::warn!(
            entries = failed.len(),
            retry_in_secs = backoff.as_secs_f64(),
            "recovered recordings not delivered; retrying"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(pacing.retry_max);
        summary.retry_rounds += 1;
        round = failed.into();
    }
    if summary != Summary::default() {
        tracing::info!(
            delivered = summary.delivered,
            refused = summary.refused,
            failed_attempts = summary.failed,
            removed = summary.dropped,
            left = summary.left,
            retry_rounds = summary.retry_rounds,
            "recording recovery done"
        );
    }
    summary
}

enum Unsent {
    Dropped,
    Left,
}

impl Unsent {
    fn count(self, summary: &mut Summary) {
        match self {
            Self::Dropped => summary.dropped += 1,
            Self::Left => summary.left += 1,
        }
    }
}

enum Sent {
    Not(Unsent),
    Delivered,
    Refused,
    Failed,
}

/// Prepares and sends the entry `name`, then waits out what is left of
/// `interval` after a send.
async fn send_one(
    outbox: &Arc<Outbox>,
    name: &OsString,
    callbacks: &Callbacks,
    max_age: Duration,
    interval: Duration,
) -> Sent {
    let prepared = {
        let outbox = outbox.clone();
        let name = name.clone();
        tokio::task::spawn_blocking(move || prepare(&outbox, &name, max_age, SystemTime::now()))
            .await
    };
    let Ok(prepared) = prepared else {
        return Sent::Not(Unsent::Left);
    };
    let (path, body, entry, interrupted) = match prepared {
        Prepared::Leave { why } => {
            tracing::warn!(why, "recording outbox entry left unsent");
            return Sent::Not(Unsent::Left);
        }
        Prepared::Drop { file, why } => {
            tracing::info!(file, why, "recording outbox entry removed unsent");
            let outbox = outbox.clone();
            let _ = tokio::task::spawn_blocking(move || outbox.remove(&file)).await;
            return Sent::Not(Unsent::Dropped);
        }
        Prepared::Send {
            path,
            body,
            entry,
            interrupted,
        } => (path, body, entry, interrupted),
    };
    let next = Instant::now() + interval;
    let file = entry.file.clone();
    let outcome =
        match callbacks.send_recovered(&path, body, OutboxEntry::new(outbox.clone(), entry)) {
            Some(done) => done.await.unwrap_or(Delivery::Failed),
            None => Delivery::Failed,
        };
    tracing::info!(file, interrupted, outcome = ?outcome, "recovered recording reported");
    tokio::time::sleep_until(next).await;
    match outcome {
        Delivery::Delivered => Sent::Delivered,
        Delivery::Refused => Sent::Refused,
        Delivery::Failed => Sent::Failed,
    }
}

fn prepare(
    outbox: &Outbox,
    name: &std::ffi::OsStr,
    max_age: Duration,
    now: SystemTime,
) -> Prepared {
    let (entry, _) = match outbox.read(name) {
        Ok(read) => read,
        Err(ReadError::Missing) => return Prepared::Leave { why: "gone" },
        Err(ReadError::Newer) => {
            return Prepared::Leave {
                why: "written by a newer sapflow",
            };
        }
        Err(_) => {
            return Prepared::Leave {
                why: "unreadable; expiry removes it",
            };
        }
    };
    let drop = |why| Prepared::Drop {
        file: entry.file.clone(),
        why,
    };
    let Some(path) = entry.stream_path() else {
        return Prepared::Leave {
            why: "unreadable; expiry removes it",
        };
    };
    let Some(tracks) = RecordingTracks::new(entry.has_video, entry.has_audio) else {
        return Prepared::Leave {
            why: "unreadable; expiry removes it",
        };
    };
    let mut recording = match outbox.open_recording(&entry.file) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return drop("recording gone");
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return drop("not a regular file");
        }
        Err(_) => {
            return Prepared::Leave {
                why: "recording cannot be opened",
            };
        }
    };
    let Ok(meta) = recording.metadata() else {
        return Prepared::Leave {
            why: "recording cannot be read",
        };
    };
    let expired = meta
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > max_age);
    if expired {
        return drop("recording expired");
    }
    let interrupted = entry.finished.is_none();
    let (complete, reason, duration) = match &entry.finished {
        Some(finished) => (
            finished.complete,
            finished.reason.clone(),
            finished.duration_ms,
        ),
        None => {
            let prefix = match sapflow_hls::fmp4::recorded_prefix(
                &mut recording,
                meta.len(),
                entry.audio_rate,
            ) {
                Ok(prefix) => prefix,
                Err(_) => {
                    return Prepared::Leave {
                        why: "recording cannot be read",
                    };
                }
            };
            if prefix.fragments == 0 {
                return drop("nothing decodable");
            }
            (
                false,
                Some(IncompleteReason::ServerInterrupted.as_str().to_owned()),
                duration_ms(prefix.media_end),
            )
        }
    };
    let Some(body) = completion_body(
        &path,
        outbox.segment_path(&entry.file),
        duration as f64 / 1000.0,
        tracks,
        entry.scoped.as_ref(),
        complete,
        reason.as_deref(),
    ) else {
        return Prepared::Leave {
            why: "callback body refused",
        };
    };
    Prepared::Send {
        path,
        body,
        entry,
        interrupted,
    }
}
