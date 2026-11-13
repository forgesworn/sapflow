//! The stream-health callback to v4v. With `SAPFLOW_PUBLISH_AUTH_URL` set,
//! sapflow makes one other outbound call, the publish authorisation request
//! (`publish_auth.rs`), over the same one-shot `POST` below.
//!
//! Events go through one queue and are sent in order, so an `unpublish`
//! never overtakes the `publish` before it.
//!
//! A `recordSegmentComplete` can carry its recording's outbox entry
//! ([`crate::recording_outbox`]): the sender removes the entry once the app
//! has answered 2xx, and leaves it otherwise, for the next start to send.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sapflow_core::{StreamEvent, StreamPath, callback_url};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};

use crate::http_lite::{self, Endpoint, ParsedResponse};
use crate::publish_observer::{CallbackScope, CallbackTicket, RecordingObservation};
use crate::recording_outbox::{Outbox, OutboxEntry, ScopedIdentity};

const QUEUE: usize = 512;
const MAX_BODY: usize = 16 * 1024;
/// Connect, send and read the answer's head, per attempt.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// A recovered `recordSegmentComplete`'s attempt. v4v copies and hashes the
/// whole file before it answers: a four-hour show at its top recommended
/// bitrate (4,000 kbit/s, about 2 GB an hour) is 8 GB, which storage copying
/// at 20 MB/s takes about seven minutes over. Ten minutes covers that with a
/// margin. It holds only the recovered lane, never live callbacks.
pub(crate) const RECOVERED_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(600);
/// Queued recovered completions: recovery sends one at a time.
const RECOVERED_QUEUE: usize = 4;
/// How much of a 408, 429 or 503 body is read, for its `retryAfterMs`.
const MAX_HINT_BODY: usize = 1024;
/// How long that body read may take, apart from the attempt's own limit.
const HINT_READ_TIMEOUT: Duration = Duration::from_millis(1_500);

/// How one sender sends: its attempts per event, each attempt's limit, and
/// the longest wait a `Retry-After` or `retryAfterMs` hint can ask for.
#[derive(Debug, Clone, Copy)]
struct Lane {
    attempts: u32,
    attempt_timeout: Duration,
    max_hint: Duration,
}

/// Live events: four attempts of five seconds, and hints honoured up to
/// 10 s, so one event holds the queue at most about 50 s (three waits and
/// four attempts), not minutes.
const LIVE: Lane = Lane {
    attempts: 4,
    attempt_timeout: ATTEMPT_TIMEOUT,
    max_hint: Duration::from_secs(10),
};
/// Recovered completions: their own sender, so a slow import never holds up
/// a live `publish`, two long attempts, and hints honoured up to a minute;
/// recovery's rounds retry beyond.
const RECOVERED: Lane = Lane {
    attempts: 2,
    attempt_timeout: RECOVERED_ATTEMPT_TIMEOUT,
    max_hint: Duration::from_secs(60),
};
/// Interim `1xx` answers skipped before a final one; more is a broken peer.
const MAX_INTERIM: usize = 4;

pub(crate) struct Queued {
    path: StreamPath,
    name: &'static str,
    pub(crate) body: Bytes,
    ticket: Option<CallbackTicket>,
    /// A recording's outbox entry, removed once the app has answered 2xx.
    entry: Option<OutboxEntry>,
    /// Told how the sender finished with the event.
    done: Option<oneshot::Sender<Delivery>>,
    _pending: Pending,
}

/// How the sender finished with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The app answered 2xx.
    Delivered,
    /// The app answered 4xx: not retried.
    Refused,
    /// An attempt reached the app (it answered, or timed out once
    /// connected) and none succeeded; or the URL is not valid.
    Failed,
    /// No attempt reached the app: every connection failed.
    Unreachable,
}

/// Counts one event from enqueue until the sender is done with it, delivered
/// or failed, or until it is dropped unsent. An empty channel is not enough:
/// the event being sent, through all its attempts, has left it.
struct Pending(Arc<watch::Sender<usize>>);

impl Pending {
    fn new(count: &Arc<watch::Sender<usize>>) -> Self {
        count.send_modify(|n| *n += 1);
        Self(count.clone())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

#[derive(Clone)]
pub struct Callbacks {
    tx: Option<mpsc::Sender<Queued>>,
    /// The recovered lane ([`RECOVERED`]); its events are not waited for at
    /// a stop, since what it has not delivered stays owed.
    recovered_tx: Option<mpsc::Sender<Queued>>,
    recovered_pending: Arc<watch::Sender<usize>>,
    scope: Option<Arc<CallbackScope>>,
    recording: Option<RecordingObservation>,
    /// Events queued and not yet done with, shared by every clone.
    pending: Arc<watch::Sender<usize>>,
    /// Where recordings keep their owed callbacks; only with a queue.
    outbox: Option<Arc<Outbox>>,
}

impl Callbacks {
    /// Starts the sender. With no URL configured, events are only logged.
    pub fn start(url: Option<String>, secret: Option<String>) -> Self {
        let Some(url) = url else {
            return Self::with_queue(None);
        };
        let (tx, rx) = mpsc::channel(QUEUE);
        let (recovered_tx, recovered_rx) = mpsc::channel(RECOVERED_QUEUE);
        tokio::spawn(run(url.clone(), secret.clone(), rx, LIVE));
        tokio::spawn(run(url, secret, recovered_rx, RECOVERED));
        let mut callbacks = Self::with_queue(Some(tx));
        callbacks.recovered_tx = Some(recovered_tx);
        callbacks
    }

    fn with_queue(tx: Option<mpsc::Sender<Queued>>) -> Self {
        Self {
            tx,
            recovered_tx: None,
            recovered_pending: Arc::new(watch::Sender::new(0)),
            scope: None,
            recording: None,
            pending: Arc::new(watch::Sender::new(0)),
            outbox: None,
        }
    }

    /// Recordings keep their owed callbacks in `outbox`. Without a callback
    /// URL nothing is owed, and no outbox is kept.
    pub fn with_outbox(mut self, outbox: Arc<Outbox>) -> Self {
        if self.tx.is_some() {
            self.outbox = Some(outbox);
        }
        self
    }

    pub(crate) fn outbox(&self) -> Option<&Arc<Outbox>> {
        self.outbox.as_ref()
    }

    /// The identity the completion of the recording `recording_id` carries,
    /// live or recovered: none for the legacy contract (`Some(None)`). A
    /// scoped one reserves the completion's sequence now, since after a
    /// restart there is no connection to take one from; the live completion
    /// uses the same one, so a resend is the same body. None if the sequence
    /// is exhausted.
    pub(crate) fn recovery_identity(&self, recording_id: &str) -> Option<Option<ScopedIdentity>> {
        let Some(scope) = &self.scope else {
            return Some(None);
        };
        Some(Some(ScopedIdentity {
            event_generation: scope.key().event_generation.get(),
            session_id: scope.key().session_id.as_str().to_owned(),
            connection_epoch: scope.connection_epoch(),
            event_sequence: scope.next_sequence()?,
            recording_id: recording_id.to_owned(),
        }))
    }

    pub fn scoped(&self, scope: Arc<CallbackScope>) -> Self {
        Self {
            tx: self.tx.clone(),
            recovered_tx: self.recovered_tx.clone(),
            recovered_pending: self.recovered_pending.clone(),
            scope: Some(scope),
            recording: None,
            pending: self.pending.clone(),
            outbox: self.outbox.clone(),
        }
    }

    /// Events queued that the sender has not finished with yet.
    pub fn pending(&self) -> usize {
        *self.pending.borrow()
    }

    /// Waits until every queued event has been delivered or has failed. Events
    /// queued meanwhile are waited for too. The caller bounds the wait: one
    /// event can take four attempts of up to five seconds each.
    pub async fn drained(&self) {
        let mut pending = self.pending.subscribe();
        let _ = pending.wait_for(|n| *n == 0).await;
    }

    pub(crate) fn for_recording(&self) -> Result<(Self, Option<RecordingObservation>), ()> {
        let Some(scope) = &self.scope else {
            return Ok((self.clone(), None));
        };
        let recording = scope.recording().ok_or(())?;
        let mut callbacks = self.clone();
        callbacks.recording = Some(recording.clone());
        Ok((callbacks, Some(recording)))
    }

    pub fn send(&self, path: &StreamPath, event: StreamEvent) {
        self.send_inner(path, event, None);
    }

    /// Freeze the writer's synchronised result into both callback contracts.
    /// The legacy event name alone only says that a file has finished.
    pub(crate) fn send_recording(
        &self,
        path: &StreamPath,
        event: StreamEvent,
        complete: bool,
        reason: Option<&str>,
    ) {
        self.send_inner(path, event, Some((complete, reason)));
    }

    /// The body of a recording's completion that has an outbox entry, with
    /// the sequence its recording reserved, decided before the entry is
    /// rewritten: [`enqueue_recording`](Self::enqueue_recording) then sends
    /// it. None, with the callback counted failed, if it is refused, which
    /// is for good: the caller removes the entry.
    pub(crate) fn recording_body(
        &self,
        path: &StreamPath,
        event: &StreamEvent,
        complete: bool,
        reason: Option<&str>,
        sequence: Option<u64>,
    ) -> Option<Bytes> {
        tracing::info!(path = %path.as_path(), event = event.name(), "stream event");
        let body = self.tx.as_ref().and_then(|_| {
            let _order = self.scope.as_ref().map(|scope| scope.event_lock());
            self.completion(path, event, complete, reason, sequence)
        });
        if body.is_none() {
            if let Some(scope) = &self.scope {
                scope.observations.callback_failed();
            }
            tracing::warn!(event = event.name(), "callback body refused");
        }
        body
    }

    /// Queues what [`recording_body`](Self::recording_body) decided, with
    /// its entry, removed on delivery.
    pub(crate) fn enqueue_recording(&self, path: &StreamPath, body: Bytes, entry: OutboxEntry) {
        let _order = self.scope.as_ref().map(|scope| scope.event_lock());
        self.enqueue(path, "recordSegmentComplete", body, false, Some(entry));
    }

    /// Queues a completion recovered at start ([`crate::recording_recovery`]),
    /// its body as [`completion_body`] made it, on the recovered lane. None
    /// if the lane refused it.
    pub(crate) fn send_recovered(
        &self,
        path: &StreamPath,
        body: Bytes,
        entry: OutboxEntry,
    ) -> Option<oneshot::Receiver<Delivery>> {
        let tx = self.recovered_tx.as_ref()?;
        let (done, outcome) = oneshot::channel();
        tracing::info!(path = %path.as_path(), file = entry.file(), "recovered recording event");
        tx.try_send(Queued {
            path: path.clone(),
            name: "recordSegmentComplete",
            body,
            ticket: None,
            entry: Some(entry),
            done: Some(done),
            _pending: Pending::new(&self.recovered_pending),
        })
        .ok()?;
        Some(outcome)
    }

    fn send_inner(
        &self,
        path: &StreamPath,
        event: StreamEvent,
        completion: Option<(bool, Option<&str>)>,
    ) {
        tracing::info!(path = %path.as_path(), event = event.name(), "stream event");
        // Allocation, sequencing and enqueue are serialised per immutable
        // scope: concurrent recorder/health tasks cannot enqueue sequence N+1
        // ahead of N. A recording's completion with an outbox entry is the
        // exception: it carries the sequence its recording reserved when the
        // file was created, so it can be below telemetry already delivered;
        // receivers must never fence recordings by sequence. No network or
        // disk operation runs under this lock.
        let _order = self.scope.as_ref().map(|scope| scope.event_lock());
        let health = matches!(event, StreamEvent::Health(_));
        if self.tx.is_none() {
            if let Some(scope) = &self.scope
                && scope.observations.callbacks_enabled()
            {
                scope.observations.callback_failed();
            }
            return;
        }
        let body = match completion {
            None => self.body(path, &event, None).map(Bytes::from),
            Some((complete, reason)) => self.completion(path, &event, complete, reason, None),
        };
        let Some(body) = body else {
            if let Some(scope) = &self.scope {
                scope.observations.callback_failed();
            }
            tracing::warn!(event = event.name(), "callback body refused");
            return;
        };
        self.enqueue(path, event.name(), body, health, None);
    }

    /// A completion's body: the event's, with the writer's integrity.
    fn completion(
        &self,
        path: &StreamPath,
        event: &StreamEvent,
        complete: bool,
        reason: Option<&str>,
        sequence: Option<u64>,
    ) -> Option<Bytes> {
        if !matches!(event, StreamEvent::RecordSegmentComplete { .. }) {
            return None;
        }
        let body = self.body(path, event, sequence)?;
        let mut body: serde_json::Value = serde_json::from_str(&body).ok()?;
        completion_fields(body.as_object_mut()?, complete, reason)?;
        let body = serde_json::to_string(&body).ok()?;
        (body.len() <= MAX_BODY).then(|| Bytes::from(body))
    }

    /// Queues a decided body on the live lane. The caller holds the scope's
    /// event lock.
    fn enqueue(
        &self,
        path: &StreamPath,
        name: &'static str,
        body: Bytes,
        health: bool,
        entry: Option<OutboxEntry>,
    ) {
        let Some(tx) = &self.tx else {
            return;
        };
        let ticket = self
            .scope
            .as_ref()
            .map(|scope| scope.observations.callback_ticket());
        let queued = Queued {
            path: path.clone(),
            name,
            body,
            ticket,
            entry,
            done: None,
            _pending: Pending::new(&self.pending),
        };
        if let Err(error) = tx.try_send(queued) {
            let (mut queued, unavailable) = match error {
                mpsc::error::TrySendError::Full(queued) => (queued, false),
                mpsc::error::TrySendError::Closed(queued) => (queued, true),
            };
            if let Some(ticket) = queued.ticket.take() {
                if health && !unavailable {
                    ticket.dropped_health();
                } else {
                    ticket.finish(false);
                }
            }
            tracing::warn!(
                event = name,
                health_dropped = health && !unavailable,
                unavailable,
                "callback queue full or unavailable"
            );
        }
    }

    /// The event's body; a scoped one takes the next sequence, or
    /// `sequence` if given (a recording's reserved one).
    fn body(
        &self,
        path: &StreamPath,
        event: &StreamEvent,
        sequence: Option<u64>,
    ) -> Option<String> {
        let legacy = event.callback_body(path);
        if legacy.len() > MAX_BODY {
            return None;
        }
        let Some(scope) = &self.scope else {
            return Some(legacy);
        };
        if scope.key().path != *path {
            return None;
        }
        let sequence = match sequence {
            Some(sequence) => sequence,
            None => scope.next_sequence()?,
        };
        let mut body: serde_json::Value = serde_json::from_str(&legacy).ok()?;
        let object = body.as_object_mut()?;
        scoped_fields(
            object,
            path,
            scope.key().event_generation.get(),
            scope.key().session_id.as_str(),
            scope.connection_epoch(),
            sequence,
        );
        if let StreamEvent::RecordSegmentComplete {
            recording_tracks, ..
        } = event
        {
            let recording = self.recording.as_ref()?;
            recording_fields(
                object,
                recording.id(),
                recording.completion_state()?,
                *recording_tracks,
            );
        }
        let body = serde_json::to_string(&body).ok()?;
        (body.len() <= MAX_BODY).then_some(body)
    }
}

/// The scoped contract's identity fields.
fn scoped_fields(
    object: &mut serde_json::Map<String, serde_json::Value>,
    path: &StreamPath,
    generation: u64,
    session_id: &str,
    connection_epoch: u64,
    sequence: u64,
) {
    object.insert("contractVersion".into(), 1.into());
    object.insert("eventId".into(), path.event_id().into());
    object.insert("eventGeneration".into(), generation.into());
    object.insert("sessionId".into(), session_id.into());
    object.insert("connectionEpoch".into(), connection_epoch.into());
    object.insert("eventSequence".into(), sequence.into());
}

/// A scoped completion's recording fields.
fn recording_fields(
    object: &mut serde_json::Map<String, serde_json::Value>,
    recording_id: &str,
    state: &str,
    tracks: sapflow_core::RecordingTracks,
) {
    object.insert("recordingId".into(), recording_id.into());
    object.insert("recordingState".into(), state.into());
    object.insert(
        "recordingTracks".into(),
        serde_json::json!({
            "hasVideo": tracks.has_video(),
            "hasAudio": tracks.has_audio(),
        }),
    );
}

/// Both contracts' writer integrity. None if a scoped body already says
/// otherwise: a scoped completion must also have passed the existing sync
/// receipt gate, and a conflicting writer observation is never overwritten.
fn completion_fields(
    object: &mut serde_json::Map<String, serde_json::Value>,
    complete: bool,
    reason: Option<&str>,
) -> Option<()> {
    let state = if complete { "finalised" } else { "incomplete" };
    if object
        .get("recordingState")
        .is_some_and(|value| value != state)
    {
        return None;
    }
    object.insert("recordingState".into(), state.into());
    object.insert(
        "recordingReason".into(),
        if complete { None } else { reason }.into(),
    );
    Some(())
}

/// A `recordSegmentComplete` body for a recording reported after a restart,
/// built as the live one is: the same fields, and for a scoped recording the
/// identity it had, with the sequence it reserved ([`ScopedIdentity`]).
pub(crate) fn completion_body(
    path: &StreamPath,
    segment_path: String,
    duration_secs: f64,
    recording_tracks: sapflow_core::RecordingTracks,
    scoped: Option<&ScopedIdentity>,
    complete: bool,
    reason: Option<&str>,
) -> Option<Bytes> {
    let event = StreamEvent::RecordSegmentComplete {
        segment_path,
        duration_secs,
        recording_tracks,
    };
    let legacy = event.callback_body(path);
    if legacy.len() > MAX_BODY {
        return None;
    }
    let mut body: serde_json::Value = serde_json::from_str(&legacy).ok()?;
    let object = body.as_object_mut()?;
    if let Some(scoped) = scoped {
        scoped_fields(
            object,
            path,
            scoped.event_generation,
            &scoped.session_id,
            scoped.connection_epoch,
            scoped.event_sequence,
        );
        recording_fields(
            object,
            &scoped.recording_id,
            if complete { "finalised" } else { "incomplete" },
            recording_tracks,
        );
    }
    completion_fields(object, complete, reason)?;
    let body = serde_json::to_string(&body).ok()?;
    (body.len() <= MAX_BODY).then(|| Bytes::from(body))
}

/// A queue without a publish-session scope: the legacy, global-publisher mode.
#[cfg(test)]
pub(crate) fn legacy_test_queue(capacity: usize) -> (Callbacks, mpsc::Receiver<Queued>) {
    let (tx, rx) = mpsc::channel(capacity);
    (Callbacks::with_queue(Some(tx)), rx)
}

#[cfg(test)]
pub(crate) fn test_queue(
    scope: Arc<CallbackScope>,
    capacity: usize,
) -> (Callbacks, mpsc::Receiver<Queued>) {
    let (tx, rx) = mpsc::channel(capacity);
    let mut callbacks = Callbacks::with_queue(Some(tx));
    callbacks.scope = Some(scope);
    (callbacks, rx)
}

/// Why an attempt got no status. Neither carries the error text: it may
/// contain the secret-bearing URL.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Timeout,
    Transport,
}

/// One header of a 2xx answer, as [`attempt_reading`] read it. No `Debug`:
/// the value may be a secret.
pub(crate) enum HeaderRead {
    Absent,
    /// Exactly one line with that name. Zeroed when dropped.
    One(zeroize::Zeroizing<Vec<u8>>),
    /// The header came more than once.
    Repeated,
}

/// Statuses that ask a client to come back later: failures, retried, not
/// refusals.
fn is_retry_later(status: u16) -> bool {
    matches!(status, 408 | 429 | 503)
}

/// How long a 408, 429 or 503 asks to wait: `Retry-After` in seconds, or
/// else the JSON body's `retryAfterMs` (v4v's rate limiter sends that).
fn retry_hint(head: &[u8], body: &[u8]) -> Option<Duration> {
    if let http_lite::ResponseHeader::One(value) = http_lite::response_header(head, "retry-after")
        && let Ok(text) = std::str::from_utf8(value)
        && let text = text.trim()
        && (1..=10).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_digit())
    {
        return text.parse().ok().map(Duration::from_secs);
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("retryAfterMs")?
        .as_u64()
        .map(Duration::from_millis)
}

/// One `POST`, then the answer's status, with `want` the value of that
/// header if the answer is 2xx, and with `hint` a 408, 429 or 503's retry
/// hint. The connection is plain HTTP/1.1 and used once (`connection:
/// close`); `connected` says whether it was made. Only the status line and
/// headers are read, within `MAX_HEAD`. A body is read only with `hint`
/// (the callback lanes, never publish authorisation), only of a 408, 429 or
/// 503, up to its `content-length`, at most [`MAX_HINT_BODY`] bytes and for
/// at most [`HINT_READ_TIMEOUT`]. A redirect is a status like any other:
/// the `location` is never followed.
async fn post(
    endpoint: &Endpoint,
    body: &[u8],
    want: Option<&str>,
    hint: bool,
    connected: &mut bool,
) -> std::io::Result<(u16, HeaderRead, Option<Duration>)> {
    let mut socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;
    *connected = true;
    // A receiver may answer, and close, before it has read the whole body.
    // Keep a write error to report only if no answer arrives.
    let wrote = socket
        .write_all(&endpoint.post("application/json", body))
        .await;
    // The head may carry the wanted header's secret value: zeroed on drop,
    // and sized for the whole head up front so no copy is left behind.
    let mut buf = zeroize::Zeroizing::new(Vec::with_capacity(if want.is_some() {
        http_lite::MAX_HEAD
    } else {
        512
    }));
    let mut interim = 0;
    loop {
        match http_lite::parse_response(&buf) {
            ParsedResponse::Complete { status, used }
                if (100..200).contains(&status) && status != 101 =>
            {
                interim += 1;
                if interim > MAX_INTERIM {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
                buf.drain(..used);
                continue;
            }
            ParsedResponse::Complete { status, used } => {
                let header = match want {
                    Some(name) if (200..300).contains(&status) => {
                        match http_lite::response_header(&buf[..used], name) {
                            http_lite::ResponseHeader::Absent => HeaderRead::Absent,
                            http_lite::ResponseHeader::One(value) => {
                                HeaderRead::One(zeroize::Zeroizing::new(value.to_vec()))
                            }
                            http_lite::ResponseHeader::Unusable => HeaderRead::Repeated,
                        }
                    }
                    _ => HeaderRead::Absent,
                };
                if !hint || !is_retry_later(status) {
                    return Ok((status, header, None));
                }
                let head = &buf[..used];
                let declared = match http_lite::response_header(head, "content-length") {
                    http_lite::ResponseHeader::One(value) => std::str::from_utf8(value)
                        .ok()
                        .and_then(|v| v.trim().parse::<usize>().ok()),
                    _ => None,
                };
                let want_bytes = declared.unwrap_or(MAX_HINT_BODY).min(MAX_HINT_BODY);
                let mut rest = buf[used..].to_vec();
                rest.truncate(want_bytes);
                let _ = tokio::time::timeout(HINT_READ_TIMEOUT, async {
                    while rest.len() < want_bytes {
                        let mut chunk = [0u8; 512];
                        let room = (want_bytes - rest.len()).min(chunk.len());
                        match socket.read(&mut chunk[..room]).await {
                            Ok(n) if n > 0 => rest.extend_from_slice(&chunk[..n]),
                            _ => break,
                        }
                    }
                })
                .await;
                return Ok((status, header, retry_hint(head, &rest)));
            }
            ParsedResponse::Malformed => return Err(std::io::ErrorKind::InvalidData.into()),
            ParsedResponse::Partial => {}
        }
        let mut chunk = zeroize::Zeroizing::new([0u8; 2048]);
        let room = http_lite::MAX_HEAD
            .saturating_sub(buf.len())
            .min(chunk.len());
        match socket.read(&mut chunk[..room]).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            Ok(_) => {
                return Err(wrote
                    .err()
                    .unwrap_or(std::io::ErrorKind::UnexpectedEof.into()));
            }
            Err(e) => return Err(wrote.err().unwrap_or(e)),
        }
    }
}

pub(crate) async fn attempt(
    endpoint: &Endpoint,
    body: &[u8],
    limit: Duration,
) -> Result<u16, Failure> {
    attempt_reading(endpoint, body, limit, None)
        .await
        .map(|(status, _)| status)
}

/// A callback lane's attempt: [`attempt`], also giving a 408, 429 or
/// 503's retry hint, and on failure whether the connection was made (a
/// timeout after it reached the app; a failure before it did not).
async fn attempt_hinted(
    endpoint: &Endpoint,
    body: &[u8],
    limit: Duration,
) -> Result<(u16, Option<Duration>), (Failure, bool)> {
    let mut connected = false;
    let outcome =
        tokio::time::timeout(limit, post(endpoint, body, None, true, &mut connected)).await;
    match outcome {
        Ok(Ok((status, _, hint))) => Ok((status, hint)),
        Ok(Err(_)) => Err((Failure::Transport, connected)),
        Err(_) => Err((Failure::Timeout, connected)),
    }
}

/// [`attempt`], also reading the header `want` of a 2xx answer.
pub(crate) async fn attempt_reading(
    endpoint: &Endpoint,
    body: &[u8],
    limit: Duration,
    want: Option<&str>,
) -> Result<(u16, HeaderRead), Failure> {
    let mut connected = false;
    match tokio::time::timeout(limit, post(endpoint, body, want, false, &mut connected)).await {
        Ok(Ok((status, header, _))) => Ok((status, header)),
        Ok(Err(_)) => Err(Failure::Transport),
        Err(_) => Err(Failure::Timeout),
    }
}

/// The wait before the next attempt: the lane's backoff, or longer if a
/// hint asks, up to the lane's limit.
fn hinted_wait(lane: Lane, delay: Duration, hint: Option<Duration>) -> Duration {
    hint.map_or(delay, |hint| hint.min(lane.max_hint).max(delay))
}

async fn run(template: String, secret: Option<String>, mut rx: mpsc::Receiver<Queued>, lane: Lane) {
    while let Some(mut queued) = rx.recv().await {
        let url = callback_url(&template, &queued.path, secret.as_deref());
        let mut delay = Duration::from_millis(500);
        let mut delivered = false;
        let mut refused = false;
        // Whether any attempt reached the app; an invalid URL counts, so it
        // is a failure and not "unreachable".
        let mut reached = false;
        // The URL carries the secret: it is parsed here and never logged.
        if let Some(endpoint) = Endpoint::parse(&url) {
            for attempt_number in 1..=lane.attempts {
                let mut hint = None;
                let outcome = attempt_hinted(&endpoint, &queued.body, lane.attempt_timeout).await;
                reached |= !matches!(outcome, Err((_, false)));
                match outcome {
                    Ok((status, _)) if (200..300).contains(&status) => {
                        delivered = true;
                        tracing::debug!(event = queued.name, "callback delivered");
                        break;
                    }
                    Ok((status, retry)) if is_retry_later(status) => {
                        // Busy or rate-limited, not a refusal: come back
                        // when asked, within reason.
                        hint = retry;
                        tracing::warn!(
                            event = queued.name,
                            status,
                            attempt = attempt_number,
                            retry_after_ms = retry.map(|d| d.as_millis() as u64),
                            "callback deferred"
                        );
                    }
                    Ok((status, _)) if (400..500).contains(&status) => {
                        // v4v refused it (wrong token or path): retrying won't help.
                        tracing::warn!(event = queued.name, status, "callback refused");
                        refused = true;
                        break;
                    }
                    Ok((status, _)) => {
                        tracing::warn!(
                            event = queued.name,
                            status,
                            attempt = attempt_number,
                            "callback failed"
                        );
                    }
                    Err((Failure::Transport, _)) => {
                        tracing::warn!(
                            event = queued.name,
                            attempt = attempt_number,
                            "callback transport failed"
                        );
                    }
                    Err((Failure::Timeout, _)) => {
                        tracing::warn!(
                            event = queued.name,
                            attempt = attempt_number,
                            "callback timed out"
                        );
                    }
                }
                if attempt_number < lane.attempts {
                    let wait = hinted_wait(lane, delay, hint);
                    tokio::time::sleep(wait).await;
                    delay *= 2;
                }
            }
        } else {
            reached = true;
            tracing::error!(event = queued.name, "callback URL is not valid");
        }
        if let Some(ticket) = queued.ticket.take() {
            ticket.finish(delivered);
        }
        // Removed before the event stops counting, so a stop that waited
        // for the queue leaves no entry for a delivered callback. A refused
        // or failed one stays for the next start: a restart with a wrong
        // token must not erase what is owed.
        if let Some(entry) = queued.entry.take() {
            if delivered {
                entry.remove().await;
            } else {
                tracing::warn!(
                    event = queued.name,
                    file = entry.file(),
                    "recording callback not delivered; its outbox entry is kept to send again"
                );
            }
        }
        if let Some(done) = queued.done.take() {
            let _ = done.send(if delivered {
                Delivery::Delivered
            } else if refused {
                Delivery::Refused
            } else if reached {
                Delivery::Failed
            } else {
                Delivery::Unreachable
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish_observer::fixture_scope;
    use sapflow_core::Health;

    fn health() -> StreamEvent {
        StreamEvent::Health(Health {
            bitrate_kbps: 1000,
            keyframe_interval_ms: Some(1000),
            audio: true,
            breaks: 0,
        })
    }
    fn completion() -> StreamEvent {
        StreamEvent::RecordSegmentComplete {
            segment_path: "/fixture/recording.mp4".into(),
            duration_secs: 1.0,
            recording_tracks: sapflow_core::RecordingTracks::AudioVideo,
        }
    }

    #[test]
    fn writer_integrity_is_frozen_in_both_callback_contracts() {
        for scoped in [false, true] {
            for complete in [false, true] {
                let scope = fixture_scope("a", 2, 11, 3);
                let (callbacks, mut rx) = if scoped {
                    test_queue(scope.clone(), 4)
                } else {
                    legacy_test_queue(4)
                };
                let (callbacks, observation) = callbacks.for_recording().expect("writer");
                if let Some(observation) = &observation {
                    observation.active();
                    if complete {
                        observation.finalised();
                    } else {
                        observation.synced_incomplete();
                    }
                }
                callbacks.send_recording(
                    &scope.key().path,
                    completion(),
                    complete,
                    (!complete).then_some("ingest-discarded-packets"),
                );
                let queued = rx.try_recv().expect("synchronised completion");
                let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
                assert_eq!(
                    body["recordingState"],
                    if complete { "finalised" } else { "incomplete" }
                );
                assert_eq!(
                    body["recordingReason"],
                    if complete {
                        serde_json::Value::Null
                    } else {
                        "ingest-discarded-packets".into()
                    }
                );
                assert_eq!(body.get("sessionId").is_some(), scoped);
                if let Some(observation) = observation {
                    observation.failed();
                }
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&queued.body.clone())
                        .expect("retry JSON"),
                    body
                );
            }
        }
    }

    #[test]
    fn writer_integrity_cannot_override_an_unsynchronised_or_conflicting_scope() {
        let scope = fixture_scope("a", 2, 11, 3);
        let (callbacks, mut rx) = test_queue(scope.clone(), 4);
        let (callbacks, observation) = callbacks.for_recording().expect("writer");
        let observation = observation.expect("scoped");
        observation.active();
        callbacks.send_recording(&scope.key().path, completion(), true, None);
        assert!(rx.try_recv().is_err());
        observation.synced_incomplete();
        callbacks.send_recording(&scope.key().path, completion(), true, None);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn scoped_completion_has_exact_immutable_topology_and_frozen_retry_body() {
        use sapflow_core::RecordingTracks;
        for tracks in [
            RecordingTracks::AudioOnly,
            RecordingTracks::VideoOnly,
            RecordingTracks::AudioVideo,
        ] {
            for incomplete in [false, true] {
                let scope = fixture_scope("a", 2, 11, 3);
                let (callbacks, mut rx) = test_queue(scope.clone(), 4);
                let (writer_callbacks, recording) = callbacks.for_recording().expect("writer");
                let recording = recording.expect("scoped");
                recording.active();
                if incomplete {
                    recording.synced_incomplete();
                } else {
                    recording.finalised();
                }
                writer_callbacks.send(
                    &scope.key().path,
                    StreamEvent::RecordSegmentComplete {
                        segment_path: "/fixture/recording.mp4".into(),
                        duration_secs: 1.0,
                        recording_tracks: tracks,
                    },
                );
                let queued = rx.try_recv().expect("completion");
                let expected = serde_json::json!({
                    "event": "recordSegmentComplete", "path": "live/event-a",
                    "segmentPath": "/fixture/recording.mp4", "segmentDuration": "1",
                    "contractVersion": 1, "eventId": "event-a", "eventGeneration": 2,
                    "sessionId": scope.key().session_id.as_str(), "connectionEpoch": 3,
                    "eventSequence": 1, "recordingId": recording.id(),
                    "recordingState": if incomplete { "incomplete" } else { "finalised" },
                    "recordingTracks": {"hasVideo": tracks.has_video(), "hasAudio": tracks.has_audio()},
                });
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&queued.body).expect("JSON"),
                    expected
                );
                // Later telemetry advances the shared sequencer, but retries
                // clone the queued Bytes, never reconstructing this receipt.
                callbacks.send(&scope.key().path, StreamEvent::Publish);
                let next: serde_json::Value =
                    serde_json::from_slice(&rx.try_recv().expect("later event").body)
                        .expect("JSON");
                assert_eq!(next["eventSequence"], 2);
                for _ in 0..4 {
                    let retry_body = queued.body.clone();
                    assert_eq!(retry_body.as_ptr(), queued.body.as_ptr());
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&retry_body)
                            .expect("same JSON"),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn bounded_queue_drops_health_visibly_and_fails_terminal_overflow() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, mut rx) = test_queue(scope.clone(), 1);
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        assert_eq!(scope.observations.callback_state(), "pending");
        callbacks.send(&scope.key().path, health());
        assert_eq!(scope.observations.counts().health_dropped, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 0);
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        assert_eq!(scope.observations.callback_state(), "failed");
        assert_eq!(rx.len(), 1);
        let mut first = rx.try_recv().expect("first only");
        first.ticket.take().expect("ticket").finish(true);
        assert!(rx.try_recv().is_err());
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        assert_eq!(scope.observations.callback_state(), "failed");
    }

    #[test]
    fn closed_callback_worker_is_failure_even_for_health() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, receiver) = test_queue(scope.clone(), 1);
        drop(receiver);
        callbacks.send(&scope.key().path, health());
        assert_eq!(scope.observations.callback_state(), "failed");
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        assert_eq!(scope.observations.counts().health_dropped, 0);
    }

    #[test]
    fn dropping_receiver_and_unsynchronised_completion_cannot_report_delivery() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, rx) = test_queue(scope.clone(), 2);
        let (recording_callbacks, recording) = callbacks.for_recording().expect("recording");
        let recording = recording.expect("scoped");
        recording.active();
        recording.failed();
        recording_callbacks.send(&scope.key().path, completion());
        assert_eq!(rx.len(), 0); // A failed sync cannot advertise a recording.
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        drop(rx);
        assert_eq!(scope.observations.counts().callbacks_pending, 0);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        assert_eq!(scope.observations.counts().callbacks_failed, 2);
    }

    #[test]
    fn late_recording_scope_never_uses_a_replacement_identity() {
        let old = fixture_scope("a", 1, 11, 3);
        let new = fixture_scope("a", 2, 22, 4);
        let (callbacks, mut rx) = test_queue(old.clone(), 4);
        let (recording_callbacks, recording) = callbacks.for_recording().expect("recording");
        let recording = recording.expect("scoped");
        recording.active();
        callbacks
            .scoped(new.clone())
            .send(&new.key().path, StreamEvent::Publish);
        recording.finalising();
        recording.finalised();
        recording_callbacks.send(&old.key().path, completion());
        let current: serde_json::Value =
            serde_json::from_slice(&rx.try_recv().expect("new publish").body).expect("JSON");
        let previous: serde_json::Value =
            serde_json::from_slice(&rx.try_recv().expect("late completion").body).expect("JSON");
        assert_eq!(current["eventGeneration"], 2);
        assert_eq!(current["sessionId"], new.key().session_id.as_str());
        assert_eq!(previous["eventGeneration"], 1);
        assert_eq!(previous["sessionId"], old.key().session_id.as_str());
        assert_eq!(previous["connectionEpoch"], 3);
        assert_eq!(previous["eventSequence"], 1);
        assert_eq!(previous["recordingId"], recording.id());
        assert_eq!(previous["recordingState"], "finalised");
        assert_eq!(previous["contractVersion"], 1);
        assert_eq!(previous["eventId"], "event-a");
    }

    #[test]
    fn concurrent_producers_share_one_ordered_positive_sequencer() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, mut rx) = test_queue(scope.clone(), 128);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                let callbacks = callbacks.clone();
                let path = scope.key().path.clone();
                threads.spawn(move || {
                    for _ in 0..8 {
                        callbacks.send(&path, StreamEvent::Publish);
                    }
                });
            }
        });
        for sequence in 1..=64 {
            let mut queued = rx.try_recv().expect("ordered message");
            let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
            assert_eq!(body["eventSequence"], sequence);
            queued.ticket.take().expect("ticket").finish(true);
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(scope.observations.callback_state(), "delivered");
    }

    #[test]
    fn wrong_event_and_oversized_bodies_fail_closed_but_legacy_shape_is_retained() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, rx) = test_queue(scope.clone(), 2);
        callbacks.send(
            &StreamPath::parse("live/event-other").expect("path"),
            StreamEvent::Publish,
        );
        assert_eq!(rx.len(), 0);
        assert_eq!(scope.observations.callback_state(), "failed");
        let legacy = Callbacks::with_queue(None);
        let body = legacy
            .body(&scope.key().path, &StreamEvent::Publish, None)
            .expect("legacy");
        assert_eq!(body, StreamEvent::Publish.callback_body(&scope.key().path));
        assert!(!body.contains("sessionId"));
        let completion = completion();
        let body = legacy
            .body(&scope.key().path, &completion, None)
            .expect("legacy completion");
        assert_eq!(body, completion.callback_body(&scope.key().path));
        assert_eq!(
            body,
            r#"{"event":"recordSegmentComplete","path":"live/event-a","segmentPath":"/fixture/recording.mp4","segmentDuration":"1"}"#
        );
        assert!(
            legacy
                .body(
                    &scope.key().path,
                    &StreamEvent::RecordSegmentComplete {
                        segment_path: "x".repeat(MAX_BODY),
                        duration_secs: 1.0,
                        recording_tracks: sapflow_core::RecordingTracks::AudioVideo,
                    },
                    None,
                )
                .is_none()
        );
    }

    /// What a receiver saw: one complete `POST`.
    struct Seen {
        method: String,
        target: String,
        host: Option<String>,
        content_type: Option<String>,
        connection: Option<String>,
        declared_length: Option<usize>,
        body: Vec<u8>,
    }

    /// A plain-socket stand-in for v4v, parsing requests with `httparse`
    /// separately from the client's own code. `answer` gets the index of each
    /// request and returns the raw bytes to send, or `None` to stay silent.
    struct Receiver {
        addr: std::net::SocketAddr,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Receiver {
        async fn start(answer: impl Fn(usize) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("listener");
            let addr = listener.local_addr().expect("addr");
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let answer = Arc::new(answer);
            let log = seen.clone();
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let (log, answer) = (log.clone(), answer.clone());
                    tokio::spawn(async move {
                        let Some(request) = Self::read(&mut socket).await else {
                            return;
                        };
                        let index = {
                            let mut log = log.lock().expect("log");
                            log.push(request);
                            log.len() - 1
                        };
                        match answer(index) {
                            Some(bytes) => {
                                let _ = socket.write_all(&bytes).await;
                                let _ = socket.shutdown().await;
                            }
                            None => std::future::pending::<()>().await,
                        }
                    });
                }
            });
            Self { addr, seen, task }
        }

        async fn read(socket: &mut TcpStream) -> Option<Seen> {
            let mut buf = Vec::new();
            loop {
                let mut headers = [httparse::EMPTY_HEADER; 32];
                let mut req = httparse::Request::new(&mut headers);
                if let Ok(httparse::Status::Complete(used)) = req.parse(&buf) {
                    let header = |name: &str| {
                        req.headers
                            .iter()
                            .find(|h| h.name.eq_ignore_ascii_case(name))
                            .map(|h| String::from_utf8_lossy(h.value).into_owned())
                    };
                    let declared_length = header("content-length").and_then(|v| v.parse().ok());
                    let mut body = buf[used..].to_vec();
                    while body.len() < declared_length.unwrap_or(0) {
                        let mut chunk = [0u8; 4096];
                        let n = socket.read(&mut chunk).await.ok()?;
                        if n == 0 {
                            return None;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                    return Some(Seen {
                        method: req.method?.to_owned(),
                        target: req.path?.to_owned(),
                        host: header("host"),
                        content_type: header("content-type"),
                        connection: header("connection"),
                        declared_length,
                        body,
                    });
                }
                let mut chunk = [0u8; 4096];
                let n = socket.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        }

        fn count(&self) -> usize {
            self.seen.lock().expect("log").len()
        }
        fn url(&self) -> String {
            format!("http://{}/callback", self.addr)
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::parse(&self.url()).expect("receiver URL")
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn answer_status(code: u16) -> Option<Vec<u8>> {
        Some(http_lite::response(code, &[], b""))
    }

    async fn state_is(scope: &CallbackScope, state: &str, limit: Duration) {
        tokio::time::timeout(limit, async {
            while scope.observations.callback_state() != state {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("callback state {state}"));
    }

    #[tokio::test]
    async fn real_http_ack_updates_receipts_and_client_refusal_is_terminal() {
        let receiver = Receiver::start(|hit| answer_status(if hit == 0 { 204 } else { 403 })).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), Some("fixture-token".into()))
            .scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "delivered", Duration::from_secs(2)).await;
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        state_is(&scope, "failed", Duration::from_secs(2)).await;
        assert_eq!(receiver.count(), 2);
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        // The request on the wire: what v4v has always been sent.
        let seen = receiver.seen.lock().expect("log");
        let first = &seen[0];
        assert_eq!(first.method, "POST");
        assert_eq!(first.target, "/callback?token=fixture-token");
        assert_eq!(
            first.host.as_deref(),
            Some(format!("{}", receiver.addr).as_str())
        );
        assert_eq!(first.content_type.as_deref(), Some("application/json"));
        assert_eq!(first.connection.as_deref(), Some("close"));
        assert_eq!(first.declared_length, Some(first.body.len()));
        let body: serde_json::Value = serde_json::from_slice(&first.body).expect("JSON body");
        assert_eq!(body["event"], "publish");
        assert_eq!(body["eventSequence"], 1);
        let second: serde_json::Value = serde_json::from_slice(&seen[1].body).expect("JSON body");
        assert_eq!(second["event"], "unpublish");
        assert_eq!(second["eventSequence"], 2);
        drop(seen);
        drop(callbacks);
    }

    #[tokio::test]
    async fn real_http_server_failure_exhausts_four_attempts_without_false_delivery() {
        let receiver = Receiver::start(|_| answer_status(503)).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        drop(callbacks);
    }

    #[tokio::test]
    async fn malformed_successful_heads_exhaust_retries_without_false_delivery() {
        let receiver = Receiver::start(|hit| {
            Some(match hit % 4 {
                0 => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n".to_vec(),
                1 => b"HTTP/1.1 200 OK\r\nContent-Length: nope\r\n\r\n".to_vec(),
                2 => b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551616\r\n\r\n".to_vec(),
                _ => b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
            })
        })
        .await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        drop(callbacks);
    }

    #[tokio::test]
    async fn a_redirect_is_a_failed_attempt_and_is_never_followed() {
        let elsewhere = Receiver::start(|_| answer_status(200)).await;
        let location = elsewhere.url();
        let receiver = Receiver::start(move |_| {
            Some(http_lite::response(
                302,
                &[("location", location.as_str())],
                b"",
            ))
        })
        .await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(elsewhere.count(), 0);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        drop(callbacks);
    }

    #[tokio::test]
    async fn events_are_delivered_one_at_a_time_in_order() {
        let receiver = Receiver::start(|_| answer_status(204)).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        let events = [
            StreamEvent::Publish,
            health(),
            health(),
            StreamEvent::Unpublish,
        ];
        for event in events {
            callbacks.send(&scope.key().path, event);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while receiver.count() < 4 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all four delivered");
        let sequences: Vec<u64> = receiver
            .seen
            .lock()
            .expect("log")
            .iter()
            .map(|seen| {
                serde_json::from_slice::<serde_json::Value>(&seen.body).expect("JSON")["eventSequence"]
                    .as_u64()
                    .expect("sequence")
            })
            .collect();
        assert_eq!(sequences, [1, 2, 3, 4]);
        state_is(&scope, "delivered", Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn an_event_counts_until_the_sender_is_done_with_it() {
        // Without a URL nothing is queued, so there is nothing to wait for.
        let unsent = Callbacks::start(None, None);
        unsent.send(
            &fixture_scope("a", 1, 1, 1).key().path,
            StreamEvent::Publish,
        );
        assert_eq!(unsent.pending(), 0);
        tokio::time::timeout(Duration::from_millis(50), unsent.drained())
            .await
            .expect("nothing queued");

        // Taken off the queue is not done: the sender's event in flight,
        // through all its attempts, still counts.
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, mut rx) = test_queue(scope.clone(), 4);
        let scoped = callbacks.scoped(scope.clone());
        scoped.send(&scope.key().path, StreamEvent::Publish);
        assert_eq!(callbacks.pending(), 1, "shared by every clone");
        let in_flight = rx.recv().await.expect("queued");
        assert_eq!(callbacks.pending(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), callbacks.drained())
                .await
                .is_err()
        );
        drop(in_flight);
        tokio::time::timeout(Duration::from_millis(50), callbacks.drained())
            .await
            .expect("done with");

        // A queue that refused the event never counts it.
        drop(rx);
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        assert_eq!(callbacks.pending(), 0);
    }

    #[tokio::test]
    async fn the_queue_drains_when_every_event_is_delivered_or_has_failed() {
        for (status, delivered) in [(204, true), (403, false)] {
            let receiver = Receiver::start(move |_| answer_status(status)).await;
            let scope = fixture_scope("a", 1, 1, 1);
            let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
            for event in [StreamEvent::Publish, health(), StreamEvent::Unpublish] {
                callbacks.send(&scope.key().path, event);
            }
            tokio::time::timeout(Duration::from_secs(2), callbacks.drained())
                .await
                .expect("bounded drain");
            assert_eq!(callbacks.pending(), 0);
            assert_eq!(
                receiver.count(),
                3,
                "every event answered before it is done"
            );
            let counts = scope.observations.counts();
            assert_eq!(counts.callbacks_delivered, if delivered { 3 } else { 0 });
        }
    }

    #[tokio::test]
    async fn an_invalid_callback_url_fails_at_once_without_connecting() {
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks =
            Callbacks::start(Some("http://127.0.0.1:9/a b".into()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        // No retry delays: the first backoff alone is 500 ms.
        state_is(&scope, "failed", Duration::from_millis(400)).await;
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
    }

    #[tokio::test]
    async fn interim_answers_are_skipped_but_not_without_limit() {
        let interim = || b"HTTP/1.1 100 Continue\r\n\r\n".to_vec();
        let receiver = Receiver::start(move |hit| {
            let mut bytes = interim();
            if hit == 0 {
                bytes.extend_from_slice(&http_lite::response(204, &[], b""));
            } else {
                for _ in 0..MAX_INTERIM {
                    bytes.extend_from_slice(&interim());
                }
                bytes.extend_from_slice(&http_lite::response(204, &[], b""));
            }
            Some(bytes)
        })
        .await;
        let limit = Duration::from_secs(2);
        assert_eq!(attempt(&receiver.endpoint(), b"{}", limit).await, Ok(204));
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", limit).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn an_unbounded_response_head_is_cut_off() {
        let receiver = Receiver::start(|_| {
            let mut bytes = b"HTTP/1.1 200 OK\r\nx-junk: ".to_vec();
            bytes.resize(256 * 1024, b'a');
            Some(bytes)
        })
        .await;
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn a_silent_receiver_times_out_and_a_closed_port_is_a_transport_failure() {
        let receiver = Receiver::start(|_| None).await;
        let started = std::time::Instant::now();
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_millis(150)).await,
            Err(Failure::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let closed = {
            let listener =
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
            let addr = listener.local_addr().expect("addr");
            Endpoint::parse(&format!("http://{addr}/callback")).expect("URL")
        };
        assert_eq!(
            attempt(&closed, b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn an_answer_that_is_not_http_is_a_transport_failure() {
        let receiver = Receiver::start(|_| Some(b"SSH-2.0-not-http\r\n".to_vec())).await;
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }

    #[test]
    fn a_retry_hint_comes_from_retry_after_or_v4v_s_retry_after_ms() {
        let head = |header: Option<&str>| {
            let mut head = b"HTTP/1.1 429 Too Many Requests\r\n".to_vec();
            if let Some(value) = header {
                head.extend_from_slice(format!("retry-after: {value}\r\n").as_bytes());
            }
            head.extend_from_slice(b"\r\n");
            head
        };
        let body = br#"{"ok":false,"error":"Rate limit exceeded.","retryAfterMs":1500}"#;
        assert_eq!(
            retry_hint(&head(Some("3")), body),
            Some(Duration::from_secs(3)),
            "the header first"
        );
        assert_eq!(
            retry_hint(&head(None), body),
            Some(Duration::from_millis(1500))
        );
        // An HTTP-date, or nonsense, falls back to the body, then to nothing.
        assert_eq!(
            retry_hint(&head(Some("Wed, 21 Oct 2026 07:28:00 GMT")), body),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(retry_hint(&head(Some("-1")), b"not json"), None);
        assert_eq!(retry_hint(&head(None), br#"{"retryAfterMs":"soon"}"#), None);
        assert!(is_retry_later(408) && is_retry_later(429) && is_retry_later(503));
        assert!(!is_retry_later(401) && !is_retry_later(404) && !is_retry_later(409));
    }

    #[tokio::test]
    async fn busy_and_rate_limited_answers_are_retried_when_asked_not_refused() {
        // v4v's limiter: 429 with retryAfterMs in its JSON body; then a 408
        // with Retry-After; then the delivery.
        let receiver = Receiver::start(|hit| {
            Some(match hit {
                0 => http_lite::response(
                    429,
                    &[("content-type", "application/json")],
                    br#"{"ok":false,"error":"Rate limit exceeded.","retryAfterMs":1500}"#,
                ),
                1 => http_lite::response(408, &[("retry-after", "2")], b""),
                _ => http_lite::response(204, &[], b""),
            })
        })
        .await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        let started = std::time::Instant::now();
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "delivered", Duration::from_secs(8)).await;
        assert_eq!(receiver.count(), 3);
        // Its own backoff alone would wait 0.5 s then 1 s; the hints ask
        // for 1.5 s then 2 s.
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(3_400), "{waited:?}");
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 0);
    }

    #[test]
    fn a_hint_holds_the_live_lane_seconds_and_the_recovered_lane_a_minute() {
        let hint = Some(Duration::from_secs(600));
        let backoff = Duration::from_millis(500);
        assert_eq!(hinted_wait(LIVE, backoff, hint), Duration::from_secs(10));
        assert_eq!(
            hinted_wait(RECOVERED, backoff, hint),
            Duration::from_secs(60)
        );
        assert_eq!(hinted_wait(LIVE, backoff, None), backoff);
        assert_eq!(
            hinted_wait(LIVE, backoff, Some(Duration::from_millis(1))),
            backoff,
            "never shorter than the backoff"
        );
        // One live event, answered 503 with a long hint every time, holds
        // the queue for at most its three waits and four attempts.
        let worst = LIVE.max_hint * (LIVE.attempts - 1) + LIVE.attempt_timeout * LIVE.attempts;
        assert!(worst <= Duration::from_secs(50), "{worst:?}");
    }

    /// A listener that answers `head` (and `body`) to one request and then
    /// holds the connection open.
    async fn holding(answer: Vec<u8>) -> Endpoint {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(&answer).await;
            std::future::pending::<()>().await;
        });
        Endpoint::parse(&format!("http://{addr}/callback")).expect("URL")
    }

    #[tokio::test]
    async fn a_hint_body_is_read_to_its_length_with_its_own_deadline_and_only_for_callbacks() {
        let body = br#"{"retryAfterMs":1500}"#;
        let whole = |declared: usize, sent: &[u8]| {
            let mut answer =
                format!("HTTP/1.1 429 Too Many Requests\r\ncontent-length: {declared}\r\n\r\n")
                    .into_bytes();
            answer.extend_from_slice(sent);
            answer
        };
        // The whole declared body, and the connection held open: the read
        // stops at the length, not at the close.
        let endpoint = holding(whole(body.len(), body)).await;
        let started = std::time::Instant::now();
        let answer = attempt_hinted(&endpoint, b"{}", Duration::from_secs(5)).await;
        assert!(matches!(answer, Ok((429, Some(hint))) if hint == Duration::from_millis(1500)));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        // A body that never comes in full: given up at its own deadline,
        // well inside the attempt's, with no hint.
        let endpoint = holding(whole(100, &body[..5])).await;
        let started = std::time::Instant::now();
        let answer = attempt_hinted(&endpoint, b"{}", Duration::from_secs(5)).await;
        assert!(matches!(answer, Ok((429, None))));
        let took = started.elapsed();
        assert!(
            took >= HINT_READ_TIMEOUT && took < Duration::from_secs(3),
            "{took:?}"
        );
        // Publish authorisation never reads a body: it has its status at once.
        let endpoint = holding(whole(100, &body[..5])).await;
        let started = std::time::Instant::now();
        assert_eq!(
            attempt(&endpoint, b"{}", Duration::from_secs(5)).await,
            Ok(429)
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn a_refused_connection_never_reached_the_app_but_a_timeout_after_connecting_did() {
        let closed = {
            let listener =
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
            let addr = listener.local_addr().expect("addr");
            Endpoint::parse(&format!("http://{addr}/callback")).expect("URL")
        };
        assert!(matches!(
            attempt_hinted(&closed, b"{}", Duration::from_secs(2)).await,
            Err((Failure::Transport, false))
        ));
        let silent = Receiver::start(|_| None).await;
        assert!(matches!(
            attempt_hinted(&silent.endpoint(), b"{}", Duration::from_millis(200)).await,
            Err((Failure::Timeout, true))
        ));
    }
}
