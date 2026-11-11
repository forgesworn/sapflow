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
//!   spoofed, takes it, and the publisher's media is dropped until its own
//!   next check from it, which takes it back (the other session has no
//!   fresh media from it).
//! - An admitted publisher that can spoof source addresses and whose check
//!   from a victim's tuple arrives before the victim's first one owns that
//!   tuple. If it then sends its own SRTP from that tuple, it keeps it, and
//!   the victim's handshake on that tuple fails.
//!
//! Each session's owned tuples are kept within the ones it still has
//! validated, so the map is bounded by the sessions' tuple tables.
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
use std::collections::{HashMap, VecDeque};
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
}

struct Entry {
    session: Session,
    /// Tuples this session owns, all validated by it.
    owned: Vec<Tuple>,
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
        })
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

    /// A live session.
    pub fn session(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id).map(|e| &e.session)
    }

    /// The session that owns a tuple.
    pub fn owner(&self, tuple: &Tuple) -> Option<SessionId> {
        self.owners.get(tuple).copied()
    }

    /// Adds a session. Its ufrag must be unique among live sessions.
    pub fn insert(&mut self, id: SessionId, session: Session) -> Result<(), InsertError> {
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
        let entry = self.sessions.get_mut(&id)?;
        let before = entry.session.stats().ice.successes;
        entry.session.handle_datagram(now, tuple, bytes);
        // A check the session took as new, and so validated this tuple.
        let checked =
            stun && entry.session.stats().ice.successes > before && entry.session.accepts(&tuple);
        let gone = !stun && !entry.session.accepts(&tuple);
        self.drain(id);
        if checked {
            self.claim(now, tuple, id);
        } else if gone {
            self.stats.stale_owner += 1;
            self.forget(&tuple, id);
        }
        Some(id)
    }

    /// Runs every session whose deadline has come.
    pub fn handle_timeout(&mut self, now: Instant) {
        let due: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, e)| e.session.poll_timeout().is_some_and(|d| d <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            if let Some(e) = self.sessions.get_mut(&id) {
                e.session.handle_timeout(now);
            }
            self.drain(id);
        }
    }

    /// The earliest session deadline.
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.sessions
            .values()
            .filter_map(|e| e.session.poll_timeout())
            .min()
    }

    /// Ends a session ([`Session::close`]) and removes it. Returns whether
    /// it was live.
    pub fn close(&mut self, id: &SessionId, now: Instant) -> bool {
        let Some(e) = self.sessions.get_mut(id) else {
            return false;
        };
        e.session.close(now);
        self.drain(*id);
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
            let keeps = self.sessions.get(&owner).is_some_and(|e| {
                let s = &e.session;
                s.accepts(&tuple)
                    && (matches!(s.state(), State::Checking | State::Handshaking)
                        || s.media_from(&tuple)
                            .is_some_and(|t| now.saturating_duration_since(t) < hold))
            });
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
        let Some(Entry { session, owned }) = self.sessions.get_mut(&id) else {
            return;
        };
        owned.push(tuple);
        // Keep only tuples the session still has validated.
        let mut lapsed = Vec::new();
        owned.retain(|t| {
            let keep = session.accepts(t);
            if !keep {
                lapsed.push(*t);
            }
            keep
        });
        for t in lapsed {
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
        let Some(e) = self.sessions.get_mut(&id) else {
            return;
        };
        while let Some(t) = e.session.poll_transmit() {
            self.out.push_back(t);
        }
        while let Some(ev) = e.session.poll_event() {
            self.events.push_back((id, ev));
        }
        if !matches!(e.session.state(), State::Ended(_)) {
            return;
        }
        if let Some(e) = self.sessions.remove(&id) {
            self.ufrags.remove(e.session.local_ufrag());
            for t in &e.owned {
                if self.owners.get(t) == Some(&id) {
                    self.owners.remove(t);
                }
            }
            self.stats.removed += 1;
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
