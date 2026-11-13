//! Our own SRT listener. Admission persistence runs in bounded asynchronous
//! tasks; the UDP owner continues handling timers and established publishers.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::Arc;
use std::time::Instant;

use sapflow_core::PublishRequest;
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::{CloseReason, Config, ConnId, Event, KeyCheckError, Listener, RejectCode};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::App;
use crate::publish::{Admitted, Publication, PublicationLoss};
use crate::publish_sessions::ScopedLease;
use crate::recorder::IncompleteReason;
use crate::srt::{Admission, IDLE, Refused, admit, feed_payload};
use crate::srt_replay::{ReplayGuard, Seen};
use crate::srt_replay_store::ReplayStoreError;
use crate::stream::SourceKind;

const QUEUE: usize = 16_384;
struct Payload {
    bytes: Vec<u8>,
    received: Instant,
    /// The SRT receiver gave up on data just before this payload.
    after_loss: bool,
}
struct Publisher {
    tx: mpsc::Sender<Payload>,
    scope: Option<ScopedLease>,
    loss: PublicationLoss,
}
impl Publisher {
    fn closed(self, reason: Option<CloseReason>, dropped: u64) {
        // `lost` also counts recovered loss. Only irrecoverable drops, or
        // an abnormal transport timeout, make this prefix incomplete. A peer
        // SHUTDOWN without drops is clean: senders that discard unsent data
        // on close can still cut the final audio PES short, which the
        // terminal drain may then trim. This runs before `tx` drops, so it
        // precedes that drain.
        match incomplete_close(reason, dropped) {
            Some(cause) => self.loss.mark_incomplete(cause),
            None if matches!(reason, Some(CloseReason::Shutdown)) => self.loss.mark_clean_close(),
            None => {}
        }
        if let Some(scope) = self.scope {
            scope.protocol_stopped();
        }
    }

    fn enqueue(&self, payload: Payload) -> bool {
        match self.tx.try_send(payload) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.loss.mark_incomplete(IncompleteReason::IngestQueueFull);
                false
            }
            // End/revoke may already have closed admission normally.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}
fn incomplete_close(reason: Option<CloseReason>, dropped: u64) -> Option<IncompleteReason> {
    if dropped != 0 {
        Some(IncompleteReason::TransportDrop)
    } else if matches!(reason, Some(CloseReason::Timeout)) {
        Some(IncompleteReason::TransportTimeout)
    } else {
        None
    }
}
struct Engine {
    listener: Listener,
    publishers: HashMap<ConnId, Publisher>,
}
impl Drop for Engine {
    fn drop(&mut self) {
        // Also covers listener failure/cancellation before the normal ended
        // message. Native protocol state is destroyed before its receipt says
        // stopped, and dropping senders lets independent media owners finish.
        for (id, publisher) in &self.publishers {
            publisher
                .loss
                .mark_incomplete(IncompleteReason::ListenerStopped);
            self.listener.close(*id, Instant::now());
            if let Some(scope) = &publisher.scope {
                scope.protocol_stopped();
            }
        }
    }
}

/// Receive buffer asked for per allowed publisher. Every SRT publisher shares
/// one UDP socket, so the OS default (under 1 MB on macOS, 208 KB on Linux)
/// overflows in bursts with tens of streams and drops datagrams before SRT
/// sees them.
const RCVBUF_PER_PUBLISHER: usize = 512 * 1024;
const RCVBUF_MIN: usize = 4 * 1024 * 1024;
const RCVBUF_MAX: usize = 64 * 1024 * 1024;

fn wanted_recv_buffer(max_publishers: usize) -> usize {
    max_publishers
        .saturating_mul(RCVBUF_PER_PUBLISHER)
        .clamp(RCVBUF_MIN, RCVBUF_MAX)
}

/// Binds the SRT socket with a receive buffer of up to `want` bytes. macOS
/// refuses a size above its limit, so halve until the OS accepts one (Linux
/// accepts and caps silently). Returns the socket and the size the OS reports.
fn bind_udp(addr: SocketAddr, want: usize) -> std::io::Result<(UdpSocket, StdUdpSocket, usize)> {
    let std_socket = std::net::UdpSocket::bind(addr)?;
    let mut size = want;
    while size >= 64 * 1024 {
        if rustix::net::sockopt::set_socket_recv_buffer_size(&std_socket, size).is_ok() {
            break;
        }
        size /= 2;
    }
    let got = rustix::net::sockopt::socket_recv_buffer_size(&std_socket)?;
    std_socket.set_nonblocking(true)?;
    let pending = std_socket.try_clone()?;
    Ok((UdpSocket::from_std(std_socket)?, pending, got))
}

// A fixed per-turn budget keeps hostile/continuous UDP input from starving
// ACKs, delivery, expiry, admission completion or publisher cancellation.
const TIMER_RECV_BUDGET: usize = 4096;

/// Drain a bounded ready batch before interpreting an overdue deadline. `recv`
/// must be nonblocking. Timers run even when the batch limit is reached.
fn service_ready_timers(
    listener: &mut Listener,
    buf: &mut [u8],
    mut now: impl FnMut() -> Instant,
    mut recv: impl FnMut(&mut [u8]) -> std::io::Result<(usize, SocketAddr)>,
) {
    for _ in 0..TIMER_RECV_BUDGET {
        match recv(buf) {
            Ok((n, peer)) => {
                if let Some(datagram) = buf.get(..n) {
                    listener.handle_datagram(now(), peer, datagram);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::debug!(%error, "SRT receive failed");
                break;
            }
        }
    }
    listener.handle_timeout(now());
}

fn service_due_timers(
    listener: &mut Listener,
    pending: &StdUdpSocket,
    buf: &mut [u8],
    now: impl FnMut() -> Instant,
) {
    // The nonblocking duplicate sees kernel-queued packets even before Tokio
    // has delivered a readiness notification after a scheduler stall. There
    // is still only one reader task. Tokio clears a stale readiness bit on
    // its next WouldBlock if this batch emptied the socket.
    service_ready_timers(listener, buf, now, |buf| pending.recv_from(buf));
}

pub async fn serve(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let want = wanted_recv_buffer(app.config.max_publishers);
    let (socket, pending, recv_buffer) = bind_udp(addr, want)?;
    run(app, socket, pending, recv_buffer, want).await
}

async fn run(
    app: Arc<App>,
    socket: UdpSocket,
    pending: StdUdpSocket,
    recv_buffer: usize,
    want: usize,
) -> std::io::Result<()> {
    let addr = socket.local_addr()?;
    if recv_buffer < want {
        tracing::warn!(
            requested = want,
            granted = recv_buffer,
            "SRT receive buffer below the requested size; raise the OS limit (Linux net.core.rmem_max, macOS kern.ipc.maxsockbuf) if many publishers share this server"
        );
    }
    let encryption = if app.publish_sessions.is_some() || app.config.srt_require_encryption {
        "required"
    } else if app.config.srt_crypto.is_some() || app.config.srt_app_keys {
        "optional"
    } else {
        "off"
    };
    tracing::info!(%addr,implementation="own",recv_buffer,scoped=app.publish_sessions.is_some(),encryption,app_keys=app.config.srt_app_keys,"SRT listening");
    let mut engine = Engine {
        listener: Listener::new(listener_config(&app.config), Instant::now()),
        publishers: HashMap::new(),
    };
    let mut admissions = JoinSet::new();
    // Callers whose key passed, waiting for the replay store to be written
    // before their path is claimed, and the writes.
    let mut checked = HashMap::new();
    let mut persisting = JoinSet::new();
    let (ended_tx, mut ended_rx) = mpsc::channel::<ConnId>(app.config.max_publishers.max(1));
    let mut buf = vec![0u8; 65_536];
    let timer = tokio::time::sleep(std::time::Duration::ZERO);
    tokio::pin!(timer);
    let mut armed = None;
    loop {
        let Engine {
            listener,
            publishers,
        } = &mut engine;
        let deadline = listener.poll_timeout();
        if deadline != armed {
            if let Some(d) = deadline {
                timer.as_mut().reset(d.into());
            }
            armed = deadline;
        }
        tokio::select! {
            result=socket.recv_from(&mut buf)=>{
                if let Ok((n,peer))=result && let Some(datagram)=buf.get(..n) {listener.handle_datagram(Instant::now(),peer,datagram);}
            }
            ()=&mut timer,if armed.is_some()=>{armed=None;}
            Some(id)=ended_rx.recv()=>{
                listener.close(id,Instant::now());
                if let Some(publisher)=publishers.remove(&id) && let Some(scope)=publisher.scope {scope.protocol_stopped();}
            }
            Some(result)=admissions.join_next(),if !admissions.is_empty()=>{
                if let Ok(admission)=result {
                    let mut pending=Persisting{checked:&mut checked,writes:&mut persisting};
                    complete_admission(&app,listener,publishers,&mut pending,&ended_tx,admission);
                }
            }
            Some(result)=persisting.join_next(),if !persisting.is_empty()=>{
                if let Ok((id,written))=result && let Some((peer,caller))=checked.remove(&id) {
                    complete_persisted(&app,listener,publishers,&ended_tx,(id,peer,caller,written));
                }
            }
        }
        let now = Instant::now();
        if listener
            .poll_timeout()
            .is_some_and(|deadline| deadline <= now)
        {
            service_due_timers(listener, &pending, &mut buf, Instant::now);
            armed = None;
        }
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect {
                    id,
                    peer,
                    stream_id,
                } => {
                    // Before the throttle, the app or any limit: the first
                    // caller to send key material owns it, so a copy is
                    // refused here however the original's attempt ends.
                    // Not charged to the throttle: nothing costly was done,
                    // and a replayer behind the encoder's address could
                    // otherwise lock the encoder out.
                    if app.config.srt_app_keys
                        && let Some(seen) = seen(listener, id, peer)
                        && app
                            .srt_replay
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .connect(seen, |c| listener.caller_socket(c))
                    {
                        listener.reject(id, RejectCode::BADSECRET);
                        app.log_refusal("srt", &"key material belongs to another caller");
                        continue;
                    }
                    let active = publishers.len() + admissions.len() + checked.len();
                    if active >= app.config.max_publishers {
                        listener.reject(id, RejectCode::http(402));
                        continue;
                    }
                    let app = app.clone();
                    admissions.spawn(async move {
                        let result = admit(
                            &app,
                            &String::from_utf8_lossy(&stream_id),
                            peer.ip(),
                            active,
                        )
                        .await;
                        (id, peer, result)
                    });
                }
                Event::Data {
                    id,
                    payload,
                    after_loss,
                } => {
                    if let Some(publisher) = publishers.get(&id)
                        && !publisher.enqueue(Payload {
                            bytes: payload,
                            received: Instant::now(),
                            after_loss,
                        })
                    {
                        tracing::warn!("SRT publisher queue is full or gone");
                        listener.close(id, Instant::now());
                        if let Some(publisher) = publishers.remove(&id)
                            && let Some(scope) = publisher.scope
                        {
                            scope.protocol_stopped();
                        }
                    }
                }
                Event::Closed { id, reason, stats } => {
                    if let Some(publisher) = publishers.remove(&id) {
                        publisher.closed(reason, stats.dropped);
                    }
                    if let Some(reason) = reason {
                        tracing::info!(
                            reason = match reason {
                                CloseReason::Shutdown => "shutdown",
                                CloseReason::Timeout => "timeout",
                            },
                            received = stats.received,
                            retransmitted = stats.retransmitted,
                            lost = stats.lost,
                            dropped = stats.dropped,
                            // Reordered: missing packets that came as
                            // originals, not retransmissions (for a caller
                            // that flags retransmissions, as libsrt does).
                            // nak_held of them came while their first NAK
                            // was held for reordering, so none was sent; the
                            // rest were NAKed and resent for nothing, and
                            // usually show as duplicates. Duplicates were
                            // already held; belated came after delivery or
                            // give-up, from reordering or a repeated NAK
                            // under loss. max_reorder_wait_us: the highest the
                            // reorder wait reached (0 if never); a first NAK
                            // is held no longer, and less near its deadline.
                            reordered = stats.reordered,
                            duplicates = stats.duplicates,
                            belated = stats.belated,
                            naks_sent = stats.naks_sent,
                            nak_held = stats.nak_held,
                            max_reorder_wait_us = stats.max_reorder_wait,
                            "SRT connection closed"
                        );
                    }
                }
            }
        }
        while let Some((to, datagram)) = listener.poll_transmit() {
            if socket.send_to(&datagram, to).await.is_err() {
                tracing::debug!("SRT send failed");
            }
        }
    }
}

/// Callers whose key passed, waiting for the replay store, and its writes.
struct Persisting<'a> {
    checked: &'a mut HashMap<ConnId, (SocketAddr, Checked)>,
    writes: &'a mut JoinSet<(ConnId, Result<(), ReplayStoreError>)>,
}

fn complete_admission(
    app: &Arc<App>,
    listener: &mut Listener,
    publishers: &mut HashMap<ConnId, Publisher>,
    waiting: &mut Persisting<'_>,
    ended: &mpsc::Sender<ConnId>,
    (id, peer, result): (ConnId, SocketAddr, Result<Admission, Refused>),
) {
    let (publication, scope, capacity, encryption) = match result {
        Err(error) => {
            listener.reject(id, reject_code(error));
            return;
        }
        Ok(Admission::Pending(pending)) => {
            let (caller, store) = {
                let mut replay = app.srt_replay.lock().unwrap_or_else(|e| e.into_inner());
                let Some(caller) = check_pending(app, listener, &mut replay, id, peer, pending)
                else {
                    return;
                };
                (
                    caller,
                    replay.store().map(|store| (store, replay.generation())),
                )
            };
            if let Some((store, generation)) = store {
                // Its key material is already refused to everyone else; the
                // file work runs off this loop, and the path is claimed only
                // once it is written (complete_persisted).
                waiting.checked.insert(id, (peer, caller));
                let app = app.clone();
                waiting.writes.spawn_blocking(move || {
                    (
                        id,
                        crate::srt_replay::persist(&app.srt_replay, &store, generation),
                    )
                });
                return;
            }
            let Some((publication, encryption)) = claim_checked(app, listener, id, peer, caller)
            else {
                return;
            };
            (publication, None, QUEUE, encryption)
        }
        Ok(Admission::Claimed(publication)) => {
            let scope = publication.scoped_lease();
            let encryption = match &scope {
                Some(scope) => Encryption::SessionKey(scope.crypto().map(|c| c.key_size)),
                None => deployment_encryption(&app.config, listener, id),
            };
            let (accepted, capacity) = if let Some(scope) = &scope {
                let crypto = scope.crypto();
                let capacity = scope
                    .limits()
                    .map(|(limits, _)| (limits.max_queue_bytes / 1500) as usize);
                match capacity.and_then(|capacity| {
                    scope.with_open(|| {
                        let accepted = listener.accept_with_crypto(id, Instant::now(), crypto);
                        if accepted {
                            scope.accepted();
                        }
                        (accepted, capacity)
                    })
                }) {
                    Ok(result) => result,
                    Err(_) => {
                        listener.reject(id, RejectCode::http(403));
                        scope.protocol_stopped();
                        return;
                    }
                }
            } else {
                (listener.accept(id, Instant::now()), QUEUE)
            };
            if !accepted {
                if let Some(scope) = &scope {
                    scope.protocol_stopped();
                }
                refused_key(app, peer);
                return;
            }
            (publication, scope, capacity, encryption)
        }
    };
    start(
        listener,
        publishers,
        ended,
        id,
        (publication, scope, capacity, encryption),
    );
}

/// A checked caller's replay store write has finished: claims its path and
/// accepts it, or refuses it if the write failed.
fn complete_persisted(
    app: &Arc<App>,
    listener: &mut Listener,
    publishers: &mut HashMap<ConnId, Publisher>,
    ended: &mpsc::Sender<ConnId>,
    (id, peer, caller, written): (ConnId, SocketAddr, Checked, Result<(), ReplayStoreError>),
) {
    if let Err(error) = written {
        // A restart would forget this caller's key material, so a recorded
        // copy of its connection could be replayed then: refused, nothing
        // claimed.
        tracing::error!(%error, "SRT replay store not written; caller refused");
        listener.reject(id, RejectCode::http(503));
        return;
    }
    if let Some((publication, encryption)) = claim_checked(app, listener, id, peer, caller) {
        start(
            listener,
            publishers,
            ended,
            id,
            (publication, None, QUEUE, encryption),
        );
    }
}

/// Starts an accepted publisher's media task.
fn start(
    listener: &Listener,
    publishers: &mut HashMap<ConnId, Publisher>,
    ended: &mpsc::Sender<ConnId>,
    id: ConnId,
    (publication, scope, capacity, encryption): (
        Publication,
        Option<ScopedLease>,
        usize,
        Encryption,
    ),
) {
    let (tx, rx) = mpsc::channel(capacity.clamp(1, QUEUE));
    let loss = publication.loss_notice();
    publishers.insert(id, Publisher { tx, scope, loss });
    let (encryption, key_len) = encryption.label();
    tracing::info!(
        latency_ms = listener.latency(id).map(|d| d.as_millis() as u64),
        encryption,
        key_len,
        "SRT publisher accepted"
    );
    tokio::spawn(publish(id, publication, rx, ended.clone()));
}

/// A caller refused at accept: a failed attempt against its address's
/// throttle, and a rate-limited log line.
fn refused_key(app: &Arc<App>, peer: SocketAddr) {
    app.auth_throttle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .failed(peer.ip(), Instant::now());
    app.log_refusal(
        "srt",
        &"passphrase, key length or admission handshake refused",
    );
}

/// An admission under `SAPFLOW_SRT_APP_KEYS` with no replay store (tests):
/// [`check_pending`] makes sure the caller will be accepted, then
/// [`claim_checked`] claims the path and accepts, with no await in between.
/// The SRT loop does the same, but with a store it writes the store off the
/// loop between the two.
/// SRT claims take over a live path, so a caller refused for its key, or for
/// sending none, is refused before the claim, and a live or held stage, its
/// recording and its callbacks are untouched. With the app's passphrase the
/// caller must have used it; without one the listener's own settings decide,
/// as [`Listener::accept`] would. Either way, key material that already
/// admitted another connection is a replayed handshake, refused before the
/// claim too ([`crate::srt_replay`]).
#[cfg(test)]
fn accept_pending(
    app: &Arc<App>,
    listener: &mut Listener,
    replay: &mut ReplayGuard,
    id: ConnId,
    peer: SocketAddr,
    pending: crate::srt::Pending,
) -> Option<(Publication, Encryption)> {
    let caller = check_pending(app, listener, replay, id, peer, pending)?;
    claim_checked(app, listener, id, peer, caller)
}

/// A caller whose key passed and whose key material is remembered, not yet
/// claimed for.
struct Checked {
    req: PublishRequest,
    admitted: Admitted,
    key: Option<CryptoConfig>,
    encryption: Encryption,
}

/// [`accept_pending`]'s first part: the key check and the replay guard, up
/// to remembering the caller's key material; `None` when it was refused or
/// is gone. With a store, write it ([`crate::srt_replay::persist`]) before
/// [`claim_checked`].
fn check_pending(
    app: &Arc<App>,
    listener: &mut Listener,
    replay: &mut ReplayGuard,
    id: ConnId,
    peer: SocketAddr,
    pending: crate::srt::Pending,
) -> Option<Checked> {
    let crate::srt::Pending { req, admitted, key } = pending;
    let checked = match &key {
        Some(key) => match listener.check_key(id, key) {
            Ok(size) => Ok(Encryption::AppKey(size)),
            Err(error) => {
                if error != KeyCheckError::NotPending {
                    // The error names the failure only, never the key.
                    app.log_refusal("srt", &error);
                }
                Err(error.reject_code())
            }
        },
        None => deployment_check(app, listener, id),
    };
    let encryption = match checked {
        Ok(encryption) => encryption,
        // Not answered within the accept timeout while the app was asked:
        // the caller is gone, and nothing was claimed for it.
        Err(None) => {
            app.log_refusal("srt", &"caller gave up before the app answered");
            return None;
        }
        Err(Some(code)) => {
            listener.reject(id, code);
            app.auth_throttle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .failed(peer.ip(), Instant::now());
            return None;
        }
    };
    // The caller owns its key material since it connected (checked then).
    // Once more before the claim, in case a connection with it was admitted
    // meanwhile. A caller whose key passed but cannot be told apart is
    // refused, never admitted unguarded (unreachable: the check has already
    // parsed its key material).
    let seen = seen(listener, id, peer);
    let keyed = !matches!(encryption, Encryption::None);
    if (keyed && seen.is_none()) || seen.as_ref().is_some_and(|seen| replay.admitted(seen)) {
        // As a wrong passphrase: the replayer learns nothing new.
        listener.reject(id, RejectCode::BADSECRET);
        app.log_refusal("srt", &"key material already admitted another connection");
        return None;
    }
    // Remembered before the claim, so a copy is refused from now on; with a
    // store, the claim waits until it is written too, since once this
    // caller's media can reach the stage a restart must not forget its key
    // material.
    if let Some(seen) = seen {
        replay.remember(&req.path.as_path(), seen, |c| listener.caller_socket(c));
    }
    Some(Checked {
        req,
        admitted,
        key,
        encryption,
    })
}

/// [`accept_pending`]'s second part: claims the path and accepts the caller,
/// with no await in between, if it is still waiting.
fn claim_checked(
    app: &Arc<App>,
    listener: &mut Listener,
    id: ConnId,
    peer: SocketAddr,
    caller: Checked,
) -> Option<(Publication, Encryption)> {
    let Checked {
        req,
        admitted,
        key,
        encryption,
    } = caller;
    if listener.sent_key_material(id).is_none() {
        // Gone while its key material was written: nothing to claim for.
        app.log_refusal("srt", &"caller gave up before it could be admitted");
        return None;
    }
    let publication = match Publication::claim_admitted(app, &req, SourceKind::Ts, admitted) {
        Ok(publication) => publication,
        Err(refusal) => {
            listener.reject(id, reject_code(Refused::Policy(refusal)));
            app.log_refusal("srt", &refusal);
            return None;
        }
    };
    let accepted = match &key {
        Some(key) => listener.accept_with_key(id, Instant::now(), key),
        None => listener.accept(id, Instant::now()),
    };
    if !accepted {
        // As after any failed accept: dropping the unaccepted publication
        // releases its claim.
        drop(publication);
        refused_key(app, peer);
        return None;
    }
    Some((publication, encryption))
}

/// A waiting caller's key material as the replay guard compares it; `None`
/// when it sent none (or is no longer waiting).
fn seen(listener: &Listener, id: ConnId, peer: SocketAddr) -> Option<Seen> {
    listener
        .pending_key_material(id)
        .and_then(sapflow_srt::crypto::bound_key_material)
        .zip(listener.caller_socket(id))
        .map(|(bound, socket)| Seen::new(bound, peer, socket, id))
}

/// Whether [`Listener::accept`] will admit a caller the app admitted without
/// a passphrase, decided before anything is claimed for it: key material
/// must unwrap with the global passphrase at its key length, and a caller
/// with none needs plaintext allowed. `Err(None)` when the caller is gone,
/// else the code to refuse it with (logged here).
fn deployment_check(
    app: &Arc<App>,
    listener: &Listener,
    id: ConnId,
) -> Result<Encryption, Option<RejectCode>> {
    let config = &app.config;
    let refuse = |code: RejectCode, reason: &str| {
        app.log_refusal("srt", &reason);
        Err(Some(code))
    };
    match (listener.sent_key_material(id), &config.srt_crypto) {
        (None, _) => Err(None),
        (Some(false), crypto) => {
            let plaintext = !(config.control.is_some() || config.srt_require_encryption)
                && (crypto.is_none() || config.srt_allow_plaintext);
            if plaintext {
                Ok(Encryption::None)
            } else {
                refuse(
                    RejectCode::UNSECURE,
                    "no key material, and encryption is required",
                )
            }
        }
        (Some(true), None) => refuse(
            RejectCode::UNSECURE,
            "key material, but neither the app nor the server has a passphrase",
        ),
        (Some(true), Some(global)) => match listener.check_key(id, global) {
            Ok(size) if size == global.key_size => Ok(Encryption::DeploymentKey(size)),
            Ok(_) => refuse(
                RejectCode::BADSECRET,
                "key length differs from the server passphrase's",
            ),
            Err(KeyCheckError::NotPending) => Err(None),
            Err(error) => {
                app.log_refusal("srt", &error);
                Err(error.reject_code())
            }
        },
    }
}

/// How an accepted publisher's media is encrypted, for the accept log.
enum Encryption {
    None,
    /// The global `SAPFLOW_SRT_PASSPHRASE`.
    DeploymentKey(KeySize),
    /// The passphrase the app returned for this stream.
    AppKey(KeySize),
    /// A scoped publish session's own key.
    SessionKey(Option<KeySize>),
}

impl Encryption {
    fn label(&self) -> (&'static str, Option<u8>) {
        let len = |size: &KeySize| match size {
            KeySize::Aes128 => 16,
            KeySize::Aes256 => 32,
        };
        match self {
            Self::None => ("none", None),
            Self::DeploymentKey(size) => ("deployment-key", Some(len(size))),
            Self::AppKey(size) => ("app-key", Some(len(size))),
            Self::SessionKey(size) => ("session-key", size.as_ref().map(len)),
        }
    }
}

/// What [`Listener::accept`] will admit this caller with: the global key,
/// whose length a caller must match, or nothing. Only `optional` needs to
/// look, and a caller with no key material is told apart before any key
/// derivation.
fn deployment_encryption(
    config: &crate::config::Config,
    listener: &Listener,
    id: ConnId,
) -> Encryption {
    match &config.srt_crypto {
        None => Encryption::None,
        Some(global) if !config.srt_allow_plaintext => Encryption::DeploymentKey(global.key_size),
        Some(global) => match listener.check_key(id, global) {
            Err(KeyCheckError::NoKeyMaterial) => Encryption::None,
            _ => Encryption::DeploymentKey(global.key_size),
        },
    }
}

fn listener_config(config: &crate::config::Config) -> Config {
    Config {
        max_connections: config.max_publishers.saturating_mul(2).max(4),
        require_encryption: config.control.is_some() || config.srt_require_encryption,
        crypto: config.srt_crypto.clone(),
        allow_plaintext: config.srt_allow_plaintext,
        reorder_wait_max: config.srt_reorder_wait_max,
        app_keys: config.srt_app_keys,
        ..Config::default()
    }
}
#[cfg(test)]
pub(crate) fn listener_config_for_test(config: &crate::config::Config) -> Config {
    listener_config(config)
}

fn reject_code(reason: Refused) -> RejectCode {
    RejectCode::http(reason.status())
}

async fn cancelled(cancel: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    let Some(receiver) = cancel else {
        return std::future::pending().await;
    };
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
fn reject_stale(publication: &Publication, received: Instant, scoped: bool) -> bool {
    if scoped && received.elapsed() > std::time::Duration::from_secs(1) {
        publication
            .loss_notice()
            .mark_incomplete(IncompleteReason::StaleMedia);
        true
    } else {
        false
    }
}

async fn publish(
    id: ConnId,
    mut publication: Publication,
    mut rx: mpsc::Receiver<Payload>,
    ended: mpsc::Sender<ConnId>,
) {
    let scope = publication.scoped_lease();
    let mut cancel = scope.as_ref().map(ScopedLease::cancellation);
    loop {
        tokio::select! {
            biased;
            ()=cancelled(&mut cancel)=>break,
            result=tokio::time::timeout(IDLE,rx.recv())=>match result {
                Ok(Some(data))=>{
                    if reject_stale(&publication, data.received, scope.is_some()){tracing::warn!("SRT scoped media queue exceeded one second");break}
                    if let Err(error)=feed_payload(&mut publication,&data.bytes,data.after_loss){tracing::warn!(%error,"dropping SRT publisher");break}
                }
                Ok(None)=>break,
                Err(_)=>{tracing::info!("SRT publisher idle");break}
            }
        }
    }
    publication.end();
    let _ = ended.send(id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srt::feed;
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    use sapflow_srt::seq::Seq;
    use sapflow_srt::{Caller, CallerConfig};
    use std::time::Duration;

    #[tokio::test]
    async fn queue_full_marks_gap_but_closed_worker_and_graceful_shutdown_do_not() {
        use crate::publish::loss_tests::Fixture;
        for cause in [
            "full", "closed", "shutdown", "dropped", "timeout", "age", "engine",
        ] {
            let mut fixture = Fixture::new();
            let mut publication = fixture.begin();
            Fixture::feed(&mut publication);
            let (tx, rx) = mpsc::channel(1);
            let publisher = Publisher {
                tx,
                scope: None,
                loss: publication.loss_notice(),
            };
            let incomplete = !matches!(cause, "closed" | "shutdown");
            let status = publication
                .stream
                .session()
                .recorder
                .as_ref()
                .expect("recording started")
                .status();
            match cause {
                "full" => {
                    assert!(publisher.enqueue(Payload {
                        bytes: vec![0],
                        received: Instant::now(),
                        after_loss: false,
                    }));
                    assert!(!publisher.enqueue(Payload {
                        bytes: vec![1],
                        received: Instant::now(),
                        after_loss: false,
                    }));
                }
                "closed" => {
                    drop(rx);
                    assert!(!publisher.enqueue(Payload {
                        bytes: vec![0],
                        received: Instant::now(),
                        after_loss: false,
                    }));
                }
                "shutdown" => publisher.closed(Some(CloseReason::Shutdown), 0),
                "dropped" => publisher.closed(Some(CloseReason::Shutdown), 1),
                "timeout" => publisher.closed(Some(CloseReason::Timeout), 0),
                "age" => {
                    let old = Instant::now() - Duration::from_secs(2);
                    assert!(!reject_stale(&publication, Instant::now(), true));
                    assert!(!reject_stale(&publication, old, false));
                    assert!(reject_stale(&publication, old, true));
                }
                _ => {
                    let mut engine = Engine {
                        listener: Listener::new(Config::default(), Instant::now()),
                        publishers: HashMap::new(),
                    };
                    // Engine Drop must report failure before closing the sender.
                    engine.publishers.insert(ConnId(1), publisher);
                    drop(engine);
                }
            }
            let expected = match cause {
                "full" => Some(IncompleteReason::IngestQueueFull),
                "dropped" => Some(IncompleteReason::TransportDrop),
                "timeout" => Some(IncompleteReason::TransportTimeout),
                "age" => Some(IncompleteReason::StaleMedia),
                "engine" => Some(IncompleteReason::ListenerStopped),
                _ => None,
            };
            assert_eq!(status.reason(), expected, "{cause}");
            // Only a peer SHUTDOWN without drops allows tail trimming.
            assert_eq!(
                publication.stream.session().closed_cleanly(),
                cause == "shutdown",
                "{cause}"
            );
            publication.end();
            fixture
                .completion(if incomplete {
                    "incomplete"
                } else {
                    "finalised"
                })
                .await;
        }
    }

    #[tokio::test]
    async fn clean_shutdown_trims_a_cut_off_final_audio_pes_but_a_dropped_close_does_not() {
        use crate::publish::loss_tests::Fixture;
        // A sender that discards unsent data on close stops mid-PES: cut the
        // generated A/V fixture three packets into its final audio PES.
        let fixture_ts = include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts");
        let packets: Vec<&[u8]> = fixture_ts.chunks_exact(188).collect();
        let pid = |p: &[u8]| (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
        let last_audio = packets
            .iter()
            .rposition(|p| pid(p) == 257 && p[1] & 0x40 != 0)
            .expect("fixture audio PES");
        let cut = packets[last_audio..]
            .iter()
            .enumerate()
            .filter(|(_, p)| pid(p) == 257)
            .nth(3)
            .map(|(i, _)| last_audio + i)
            .expect("multi-packet audio PES");
        let wire = packets[..cut].concat();
        for (dropped, state) in [(0, "finalised"), (1, "incomplete")] {
            let mut fixture = Fixture::new();
            let mut publication = fixture.begin();
            for datagram in wire.chunks(1316) {
                feed(&mut publication, datagram).expect("accepted prefix");
            }
            let (tx, _rx) = mpsc::channel(1);
            // As in serve: the close notice precedes the worker's terminal drain.
            Publisher {
                tx,
                scope: None,
                loss: publication.loss_notice(),
            }
            .closed(Some(CloseReason::Shutdown), dropped);
            publication.end();
            fixture.completion(state).await;
        }
    }

    #[test]
    fn configured_origin_accepts_matching_crypto_and_refuses_downgrade() {
        // Optional encryption also admits plaintext (profile 1), never a
        // wrong passphrase or key length.
        for (cipher, encryption) in [
            ("aes128", "required"),
            ("aes256", "required"),
            ("aes128", "optional"),
            ("aes256", "optional"),
        ] {
            for profile in 0..4 {
                let config = crate::config::Config::load(
                    |key| match key {
                        "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
                        "SAPFLOW_SRT_PASSPHRASE" => Some("transport test secret".into()),
                        "SAPFLOW_SRT_CIPHER" => Some(cipher.into()),
                        "SAPFLOW_SRT_ENCRYPTION" => Some(encryption.into()),
                        _ => None,
                    },
                    [],
                )
                .expect("origin config");
                let start = Instant::now();
                let mut listener = Listener::new(listener_config(&config), start);
                let size = if cipher == "aes128" {
                    KeySize::Aes128
                } else {
                    KeySize::Aes256
                };
                let crypto = match profile {
                    1 => None,
                    2 => Some(
                        CryptoConfig::new(b"wrong transport secret".to_vec(), size)
                            .expect("secret"),
                    ),
                    3 => Some(
                        CryptoConfig::new(
                            b"transport test secret".to_vec(),
                            if size == KeySize::Aes128 {
                                KeySize::Aes256
                            } else {
                                KeySize::Aes128
                            },
                        )
                        .expect("secret"),
                    ),
                    _ => config.srt_crypto.clone(),
                };
                let peer = "192.0.2.1:9000".parse().expect("peer");
                let source = "192.0.2.2:9001".parse().expect("source");
                let mut caller = Caller::with_ids(
                    CallerConfig {
                        crypto,
                        ..CallerConfig::default()
                    },
                    peer,
                    11,
                    Seq::new(100),
                    start,
                )
                .expect("caller");
                let mut accepted = false;
                let mut sent = false;
                let mut received = Vec::new();
                for ms in (0..1000).step_by(10) {
                    let now = start + Duration::from_millis(ms);
                    caller.handle_timeout(now);
                    listener.handle_timeout(now);
                    for _ in 0..32 {
                        let Some((_, bytes)) = caller.poll_transmit(now) else {
                            break;
                        };
                        listener.handle_datagram(now, source, &bytes);
                    }
                    for _ in 0..32 {
                        let Some(event) = listener.poll_event() else {
                            break;
                        };
                        match event {
                            Event::Connect { id, .. } => accepted |= listener.accept(id, now),
                            Event::Data { payload, .. } => received.extend(payload),
                            Event::Closed { .. } => {}
                        }
                    }
                    for _ in 0..32 {
                        let Some((_, bytes)) = listener.poll_transmit() else {
                            break;
                        };
                        caller.handle_datagram(now, peer, &bytes);
                    }
                    if caller.is_connected() && !sent {
                        caller.enqueue(now, vec![0x47; 188]).expect("media");
                        sent = true;
                    }
                }
                let admitted = profile == 0 || (profile == 1 && encryption == "optional");
                assert_eq!(accepted, admitted, "{cipher} {encryption} {profile}");
                if admitted {
                    assert_eq!(received, vec![0x47; 188]);
                } else {
                    assert!(caller.is_closed());
                    assert!(received.is_empty());
                    assert_eq!(listener.connections(), 0);
                }
                caller.stop(start + Duration::from_secs(1));
            }
        }
    }
}

#[cfg(test)]
mod recv_buffer_tests {
    use super::*;

    #[test]
    fn wanted_buffer_scales_with_publishers_within_bounds() {
        assert_eq!(wanted_recv_buffer(1), RCVBUF_MIN);
        assert_eq!(wanted_recv_buffer(16), 8 * 1024 * 1024);
        assert_eq!(wanted_recv_buffer(50), 25 * 1024 * 1024);
        assert_eq!(wanted_recv_buffer(usize::MAX), RCVBUF_MAX);
    }

    #[tokio::test]
    async fn bind_raises_the_receive_buffer_above_the_default() -> std::io::Result<()> {
        let plain = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let default = rustix::net::sockopt::socket_recv_buffer_size(&plain)?;
        let (_socket, _pending, got) =
            bind_udp(SocketAddr::from(([127, 0, 0, 1], 0)), 4 * 1024 * 1024)?;
        assert!(got > default, "granted {got}, default {default}");
        Ok(())
    }
}

#[cfg(test)]
#[path = "srt_own_io_tests.rs"]
mod io_tests;

#[cfg(test)]
#[path = "srt_app_keys_tests.rs"]
mod app_key_tests;
