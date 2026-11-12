//! The UDP owner's sans-IO core: every session on one socket.
//!
//! Datagrams are told apart by their first byte ([`classify`]):
//!
//! - **A STUN Binding request** goes to the session its USERNAME names: the
//!   part before the colon is our ufrag ([`Session::local_ufrag`]). It goes
//!   there whichever tuple it came from, since a check names its session
//!   itself, and nothing else is looked at before the session authenticates
//!   it. A request naming no live session is dropped unanswered.
//! - **Everything else** (DTLS, SRTP, SRTCP) goes to the session that owns
//!   its tuple, or is dropped. A tuple belongs to one session at a time.
//!
//! # Tuple ownership
//!
//! A check is no proof of where it came from: MESSAGE-INTEGRITY does not
//! cover the source address (RFC 8445 section 19.2), so a valid check for
//! one session can arrive from the tuple another session's publisher is
//! using, copied or spoofed. "The session whose check from it came last"
//! would then hand that publisher's tuple, and its media, to the wrong
//! session. So a session that a new check validates from tuple `T` takes
//! `T` unless its owner is still using it:
//!
//! - the owner has had fresh SRTP or SRTCP from `T` within
//!   [`RouterConfig::media_hold`] (2 s by default), or
//! - the owner has not connected yet (it is checking or handshaking, and so
//!   within its handshake deadline), so the first session to validate a
//!   tuple keeps it through its handshake.
//!
//! Otherwise `T` moves, which is what a NAT that gives a port to a new
//! publisher needs. Only a check that the session took as new moves a
//! tuple: a retransmission, a replay of a remembered ID, or a request that
//! fails authentication changes nothing. What this leaves open:
//!
//! - After a publisher has been silent from its tuple for longer than the
//!   hold, one valid check for another session from that tuple, copied or
//!   spoofed, takes it, and the publisher's media is dropped. If the
//!   session that took it has connected, the publisher's own next check
//!   from the tuple takes it back, since that session has no fresh media
//!   from it. If it has not connected, it keeps the tuple while it checks
//!   or handshakes, so the publisher's media is dropped until it connects
//!   or ends at its handshake deadline, up to 10 s after it was created,
//!   and then until the publisher's next check from the tuple takes it
//!   back (once it has ended, the tuple has no owner and the publisher's
//!   media from it is routed nowhere until that check).
//! - An admitted publisher that can spoof source addresses can take a
//!   tuple whenever its owner may lose it: before the owner's first check
//!   from it, or after the owner has been silent from it for longer than
//!   the hold. If it then sends its own SRTP from that tuple, it keeps it
//!   for as long as it does so, and the victim's handshake on that tuple
//!   fails or its media from it is dropped.
//!
//! Letting the publisher's check take a tuple back from a session that has
//! not connected would close the second half of the first item, but it
//! would let checks replayed for a session whose publisher has gone (their
//! IDs forgotten) break the handshake of a new publisher that a NAT has
//! given the same port, so the rule stays as it is.
//!
//! Each session's owned tuples are kept within the ones it still has
//! validated, so the map is bounded by the sessions' tuple tables.
//!
//! # Panics
//!
//! Every call into a session runs under `catch_unwind` (the workspace
//! builds with `panic = "unwind"`), and so does every read of one the
//! router makes: each session's deadline, read as each call ends (see
//! Deadlines), and the tuple owner's state when another session's check
//! arrives from its tuple. A
//! caller reads a session the same way through [`Router::inspect`]. A
//! session whose call or read panics is dropped with its queued output, its
//! ufrag and its tuples, and its last event is `Ended(Panicked)`; every
//! other session goes on as it was. [`Router::session`] is not guarded,
//! for tests and diagnostics. The routing itself reads only a datagram's
//! first byte ([`classify`]) and, for STUN, its USERNAME
//! ([`stun::request_username`]); neither reaches session code.
//!
//! # Deadlines
//!
//! A session's deadline changes only in its own calls, so the router reads
//! it as each call ends and keeps every session's in one sorted index.
//! [`Router::poll_timeout`] is the index's first entry and
//! [`Router::handle_timeout`] runs the sessions at its front, so neither
//! reads every session, and the cost of a datagram does not grow with the
//! number of sessions beyond the index's logarithm.
//!
//! # Driving it
//!
//! As for a [`Session`]: after each [`Router::handle_datagram`],
//! [`Router::handle_timeout`], [`Router::insert`] or [`Router::close`], take
//! every [`Router::poll_transmit`] and [`Router::poll_event`]. Each call
//! moves what its session queued into the router's queues, so they hold at
//! most one call's output if drained after each. A session that ends is
//! removed once its output is queued; its `Ended` event is the last one for
//! its ID.

use crate::ice::{Kind, Tuple, classify};
use crate::resource::SessionId;
use crate::session::{Event, Session, SessionError, State, Transmit};
use crate::stun;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Bounds and the ownership hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterConfig {
    /// Most sessions at once. 1 to 4096; default 16.
    pub max_sessions: usize,
    /// How long fresh media from a tuple keeps it with its session against
    /// a check for another session. 100 ms to 60 s; default 2 s.
    pub media_hold: Duration,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_sessions: 16,
            media_hold: Duration::from_secs(2),
        }
    }
}

impl RouterConfig {
    /// Checks each setting's range.
    pub fn validate(&self) -> Result<(), SessionError> {
        if !(1..=4096).contains(&self.max_sessions) {
            return Err(SessionError::Config("max_sessions: 1 to 4096"));
        }
        if !(Duration::from_millis(100)..=Duration::from_secs(60)).contains(&self.media_hold) {
            return Err(SessionError::Config("media_hold: 100 ms to 60 s"));
        }
        Ok(())
    }
}

/// Why [`Router::insert`] refused a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertError {
    /// [`RouterConfig::max_sessions`] are live.
    Full,
    /// A live session has this ID.
    DuplicateId,
    /// A live session has this local ufrag.
    DuplicateUfrag,
}

/// Counters. None holds an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RouterStats {
    /// Datagrams for no session: a request naming no live session, other
    /// STUN, or anything from a tuple no session owns.
    pub unrouted: u64,
    /// Tuples given to a session that had none, or taken from another.
    pub claims: u64,
    /// Of those, tuples taken from another session.
    pub moved: u64,
    /// Checks that validated a tuple for one session while another kept
    /// it.
    pub kept: u64,
    /// Datagrams from a tuple whose owner no longer has it validated; the
    /// tuple is then forgotten.
    pub stale_owner: u64,
    /// Sessions removed after they ended.
    pub removed: u64,
    /// Sessions dropped because a call into them, or a read of them,
    /// panicked.
    pub panics: u64,
}

struct Entry {
    session: Session,
    /// Tuples this session owns, all validated by it.
    owned: Vec<Tuple>,
    /// Its deadline as its last call ended: its key in `Router::due`.
    due: Option<Instant>,
}

/// Every session on one socket. See the module docs.
pub struct Router {
    config: RouterConfig,
    sessions: HashMap<SessionId, Entry>,
    ufrags: HashMap<String, SessionId>,
    owners: HashMap<Tuple, SessionId>,
    out: VecDeque<Transmit>,
    events: VecDeque<(SessionId, Event)>,
    stats: RouterStats,
    /// Every live session's deadline, earliest first (module docs).
    due: BTreeSet<(Instant, SessionId)>,
    /// Test builds only: whether sessions get their panic hooks turned on
    /// ([`Router::enable_panic_hooks`]).
    #[cfg(any(test, feature = "test-panic"))]
    panic_hooks: bool,
}

impl Router {
    /// An empty router.
    pub fn new(config: RouterConfig) -> Result<Self, SessionError> {
        config.validate()?;
        Ok(Self {
            config,
            sessions: HashMap::new(),
            ufrags: HashMap::new(),
            owners: HashMap::new(),
            out: VecDeque::new(),
            events: VecDeque::new(),
            stats: RouterStats::default(),
            due: BTreeSet::new(),
            #[cfg(any(test, feature = "test-panic"))]
            panic_hooks: false,
        })
    }

    /// Test builds only: turns the panic hooks on for every session here
    /// now and every one inserted later ([`Session::enable_panic_hooks`]).
    /// Off, as every router starts, the hooks do nothing.
    #[cfg(any(test, feature = "test-panic"))]
    pub fn enable_panic_hooks(&mut self) {
        self.panic_hooks = true;
        for e in self.sessions.values_mut() {
            e.session.enable_panic_hooks();
        }
    }

    /// Live sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Counters.
    pub fn stats(&self) -> RouterStats {
        self.stats
    }

    /// A live session. Reads through this are not guarded (module docs):
    /// the UDP owner reads through [`Router::inspect`].
    pub fn session(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id).map(|e| &e.session)
    }

    /// Reads a live session under `catch_unwind`. If `f` panics, the
    /// session is dropped as for a call (module docs) and `None` is
    /// returned; drain [`Router::poll_event`] after.
    pub fn inspect<R>(&mut self, id: &SessionId, f: impl FnOnce(&Session) -> R) -> Option<R> {
        self.guarded(*id, |s| f(s))
    }

    /// The session that owns a tuple.
    pub fn owner(&self, tuple: &Tuple) -> Option<SessionId> {
        self.owners.get(tuple).copied()
    }

    /// Adds a session. Its ufrag must be unique among live sessions.
    pub fn insert(&mut self, id: SessionId, session: Session) -> Result<(), InsertError> {
        #[cfg(any(test, feature = "test-panic"))]
        let session = {
            let mut session = session;
            if self.panic_hooks {
                session.enable_panic_hooks();
            }
            session
        };
        if self.sessions.len() >= self.config.max_sessions {
            return Err(InsertError::Full);
        }
        if self.sessions.contains_key(&id) {
            return Err(InsertError::DuplicateId);
        }
        if self.ufrags.contains_key(session.local_ufrag()) {
            return Err(InsertError::DuplicateUfrag);
        }
        self.ufrags.insert(session.local_ufrag().to_owned(), id);
        self.sessions.insert(
            id,
            Entry {
                session,
                owned: Vec::new(),
                due: None,
            },
        );
        self.drain(id);
        Ok(())
    }

    /// One datagram from `tuple`. Returns the session it went to.
    pub fn handle_datagram(
        &mut self,
        now: Instant,
        tuple: Tuple,
        bytes: &[u8],
    ) -> Option<SessionId> {
        let stun = classify(bytes) == Kind::Stun;
        let id = if stun {
            stun::request_username(bytes)
                .and_then(local_ufrag)
                .and_then(|u| self.ufrags.get(u))
                .copied()
        } else {
            self.owners.get(&tuple).copied()
        };
        let Some(id) = id else {
            self.stats.unrouted += 1;
            return None;
        };
        let handled = self.guarded(id, |s| {
            let before = s.stats().ice.successes;
            s.handle_datagram(now, tuple, bytes);
            // A check the session took as new, and so validated this tuple.
            let checked = stun && s.stats().ice.successes > before && s.accepts(&tuple);
            (checked, !stun && !s.accepts(&tuple))
        });
        // The session panicked and is gone; the datagram was still its.
        let Some((checked, gone)) = handled else {
            return Some(id);
        };
        self.drain(id);
        if checked {
            self.claim(now, tuple, id);
        } else if gone {
            self.stats.stale_owner += 1;
            self.forget(&tuple, id);
        }
        Some(id)
    }

    /// Runs every session whose deadline has come, from the index (module
    /// docs).
    pub fn handle_timeout(&mut self, now: Instant) {
        let due: Vec<SessionId> = self
            .due
            .iter()
            .take_while(|(d, _)| *d <= now)
            .map(|(_, id)| *id)
            .collect();
        for id in due {
            if self.guarded(id, |s| s.handle_timeout(now)).is_some() {
                self.drain(id);
            }
        }
    }

    /// The earliest session deadline, from the index (module docs).
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.due.first().map(|(d, _)| *d)
    }

    /// Whether the index holds exactly each live session's deadline, read
    /// now (tests and the fuzz harness).
    #[cfg(any(test, feature = "testing"))]
    pub fn deadlines_indexed(&self) -> bool {
        let mut now: BTreeSet<(Instant, SessionId)> = BTreeSet::new();
        for (id, e) in &self.sessions {
            if let Some(d) = e.session.poll_timeout() {
                now.insert((d, *id));
            }
            if e.due != e.session.poll_timeout() {
                return false;
            }
        }
        now == self.due
    }

    /// Keys a session under its deadline as its call ends.
    fn reindex(&mut self, id: SessionId, due: Option<Instant>) {
        let Some(e) = self.sessions.get_mut(&id) else {
            return;
        };
        if e.due == due {
            return;
        }
        if let Some(old) = e.due {
            self.due.remove(&(old, id));
        }
        if let Some(new) = due {
            self.due.insert((new, id));
        }
        e.due = due;
    }

    /// Asks a session's publisher for a keyframe
    /// ([`Session::request_keyframe`]).
    pub fn request_keyframe(&mut self, id: &SessionId, now: Instant) {
        if self.guarded(*id, |s| s.request_keyframe(now)).is_some() {
            self.drain(*id);
        }
    }

    /// Ends a session ([`Session::close`]) and removes it. Returns whether
    /// it was live.
    pub fn close(&mut self, id: &SessionId, now: Instant) -> bool {
        if !self.sessions.contains_key(id) {
            return false;
        }
        if self.guarded(*id, |s| s.close(now)).is_some() {
            self.drain(*id);
        }
        true
    }

    /// The next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.out.pop_front()
    }

    /// The next event, with its session.
    pub fn poll_event(&mut self) -> Option<(SessionId, Event)> {
        self.events.pop_front()
    }

    /// Gives `tuple` to `id` after a new check from it, unless its owner is
    /// still using it (module docs).
    fn claim(&mut self, now: Instant, tuple: Tuple, id: SessionId) {
        if !self.sessions.contains_key(&id) {
            return;
        }
        if let Some(owner) = self.owners.get(&tuple).copied() {
            if owner == id {
                return;
            }
            let hold = self.config.media_hold;
            // An owner whose read panics is dropped, its tuples with it.
            let keeps = self
                .guarded(owner, |s| {
                    s.accepts(&tuple)
                        && (matches!(s.state(), State::Checking | State::Handshaking)
                            || s.media_from(&tuple)
                                .is_some_and(|t| now.saturating_duration_since(t) < hold))
                })
                .unwrap_or(false);
            if keeps {
                self.stats.kept += 1;
                return;
            }
            if let Some(e) = self.sessions.get_mut(&owner) {
                e.owned.retain(|t| *t != tuple);
            }
            self.stats.moved += 1;
        }
        self.owners.insert(tuple, id);
        self.stats.claims += 1;
        // Keep only tuples the session still has validated. If its read
        // panics, it is dropped with every tuple it owns, this one too.
        let lapsed = self.guarded_entry(id, |Entry { session, owned, .. }| {
            owned.push(tuple);
            let mut lapsed = Vec::new();
            owned.retain(|t| {
                let keep = session.accepts(t);
                if !keep {
                    lapsed.push(*t);
                }
                keep
            });
            lapsed
        });
        for t in lapsed.unwrap_or_default() {
            if self.owners.get(&t) == Some(&id) {
                self.owners.remove(&t);
            }
        }
    }

    /// Forgets that `id` owns `tuple`.
    fn forget(&mut self, tuple: &Tuple, id: SessionId) {
        if self.owners.get(tuple) == Some(&id) {
            self.owners.remove(tuple);
        }
        if let Some(e) = self.sessions.get_mut(&id) {
            e.owned.retain(|t| t != tuple);
        }
    }

    /// Moves what a session queued into the router's queues, and removes it
    /// once it has ended.
    fn drain(&mut self, id: SessionId) {
        let drained = self.guarded(id, |s| {
            let out: Vec<Transmit> = std::iter::from_fn(|| s.poll_transmit()).collect();
            let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
            // Its new deadline, for the index (module docs).
            (
                out,
                events,
                matches!(s.state(), State::Ended(_)),
                s.poll_timeout(),
            )
        });
        let Some((out, events, ended, due)) = drained else {
            return;
        };
        self.out.extend(out);
        self.events.extend(events.into_iter().map(|ev| (id, ev)));
        if ended {
            self.remove(id);
            self.stats.removed += 1;
        } else {
            self.reindex(id, due);
        }
    }

    /// Runs `f` on a session under `catch_unwind`. If it panics, the
    /// session is dropped, since its state is not trusted after that: its
    /// queued output is discarded, its ufrag and tuples are freed, and
    /// `Ended(Panicked)` is its last event. Returns `None` then, or for a
    /// session that is not live. Every other session goes on as it was.
    fn guarded<R>(&mut self, id: SessionId, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
        self.guarded_entry(id, |e| f(&mut e.session))
    }

    /// As [`Router::guarded`], with the session's tuples.
    fn guarded_entry<R>(&mut self, id: SessionId, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let e = self.sessions.get_mut(&id)?;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(e))) {
            Ok(r) => Some(r),
            Err(_) => {
                self.fail(id);
                None
            }
        }
    }

    /// Drops a session that panicked: `Ended(Panicked)` is its last event.
    fn fail(&mut self, id: SessionId) {
        self.remove(id);
        self.stats.panics += 1;
        self.events
            .push_back((id, Event::Ended(crate::session::EndReason::Panicked)));
    }

    /// Forgets a session, its ufrag and the tuples it owns.
    fn remove(&mut self, id: SessionId) {
        if let Some(e) = self.sessions.remove(&id) {
            if let Some(d) = e.due {
                self.due.remove(&(d, id));
            }
            self.ufrags.remove(e.session.local_ufrag());
            for t in &e.owned {
                if self.owners.get(t) == Some(&id) {
                    self.owners.remove(t);
                }
            }
        }
    }
}

/// Our ufrag from a request's USERNAME (`<ours>:<theirs>`).
fn local_ufrag(username: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(username).ok()?;
    s.split_once(':').map(|(ours, _)| ours)
}

#[cfg(test)]
mod tests;
