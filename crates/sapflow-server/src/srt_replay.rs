//! Refuses SRT key material that belongs to another caller.
//!
//! A passing key check proves only that a caller's key material (its
//! `KMREQ`: salt and wrapped keys, sent in clear) was wrapped under the
//! passphrase, not who sent it. Anyone who captured an encoder's conclusion
//! handshake can replay it from another address and pass the check, and an
//! SRT claim takes over a live path: they cannot decrypt or forge media, but
//! they knock the real encoder off. Real encoders make fresh key material
//! for every connection (our `Caller`, libsrt and OBS, checked), so key
//! material seen before from another caller is a replay.
//!
//! The guard compares a SHA-256 digest of the part of the key material that
//! the passphrase check binds ([`sapflow_srt::crypto::bound_key_material`]):
//! editing anything outside it still passes the check, so it is no way round
//! the guard. It keeps two tiers, in memory, and lookups span every path:
//!
//! - Owners: when a keyed caller first connects, before the throttle or the
//!   app is consulted, its key material is owned by that caller (its address
//!   and caller socket id). The same key material from anyone else is refused
//!   at once, so a copy that arrives while the original waits for the app, or
//!   after the original was refused for any reason (throttled, the app's 401,
//!   a limit), never gets further. The owner itself may send it again: a
//!   resend after a lost rejection comes from the same address and socket.
//!   [`OWNED`] entries, oldest ended first.
//! - Admitted: key material of a caller that was accepted, refused to anyone
//!   from then on, its owner included (while its connection is open the
//!   listener answers the owner's resends on it, so a new connection with it
//!   is a copy). [`PER_PATH`] entries a path and [`TOTAL`] in all, oldest
//!   ended first, so a flood of admissions elsewhere cannot push out a live
//!   stage's entry.
//!
//! An entry whose connection is still waiting or open is never evicted, so
//! either tier may exceed its bound by the connections the listener holds.
//! There is no time limit: entries leave only by those bounds or a restart.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

use sapflow_srt::ConnId;
use sha2::{Digest, Sha256};

/// Admitted callers' key material remembered per stage path: room for far
/// more reconnects than a show sees, each one an admission with the stage's
/// passphrase.
pub const PER_PATH: usize = 128;
/// Admitted entries on all paths together: under a megabyte (an entry is
/// its digest, the path, at most 133 bytes, and the caller's address and
/// ids).
pub const TOTAL: usize = 4096;
/// Owners named at first sight, admitted or not: about 2 MB at most. Each
/// costs a keyed conclusion with a valid cookie, and the listener answers at
/// most 100 handshakes a second (bursts of 200), so pushing out an owner of
/// interest takes over two and a half minutes of every handshake the
/// listener will take, the encoder's own reconnects included.
pub const OWNED: usize = 16_384;

/// A caller's key material as the guard compares it, with who sent it.
/// Prints nothing of the key material.
pub struct Seen {
    digest: [u8; 32],
    peer: SocketAddr,
    caller_socket: u32,
    conn: ConnId,
}

impl Seen {
    /// `bound` is [`sapflow_srt::crypto::bound_key_material`] of the
    /// caller's `KMREQ`.
    pub fn new(bound: &[u8], peer: SocketAddr, caller_socket: u32, conn: ConnId) -> Self {
        Self {
            digest: Sha256::digest(bound).into(),
            peer,
            caller_socket,
            conn,
        }
    }

    fn same_caller(&self, other: &Self) -> bool {
        self.peer == other.peer && self.caller_socket == other.caller_socket
    }

    /// Whether its connection is still waiting or open, by `caller_socket`
    /// (`Listener::caller_socket`).
    fn live(&self, caller_socket: &impl Fn(ConnId) -> Option<u32>) -> bool {
        caller_socket(self.conn) == Some(self.caller_socket)
    }
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seen").finish_non_exhaustive()
    }
}

struct Entry {
    path: Box<str>,
    seen: Seen,
}

/// Key material of SRT callers, owned by the SRT loop.
#[derive(Default)]
pub struct ReplayGuard {
    owners: HashMap<[u8; 32], Seen>,
    /// The owners' digests, oldest first.
    owned_order: VecDeque<[u8; 32]>,
    /// Oldest first.
    admitted: VecDeque<Entry>,
}

impl ReplayGuard {
    /// For a keyed caller that has just connected, before anything else is
    /// done for it: whether its key material belongs to another caller (or
    /// already admitted a connection), to refuse it at once. Otherwise the
    /// caller owns it from now on, if no one did. `caller_socket` says, for
    /// a connection, the caller socket id it has while waiting or open.
    pub fn connect(&mut self, seen: Seen, caller_socket: impl Fn(ConnId) -> Option<u32>) -> bool {
        if self.admitted(&seen) {
            return true;
        }
        if let Some(owner) = self.owners.get_mut(&seen.digest) {
            if !owner.same_caller(&seen) {
                return true;
            }
            // The owner again, on a new connection: it was refused or gave
            // up, and tries once more.
            owner.conn = seen.conn;
            return false;
        }
        self.owned_order.push_back(seen.digest);
        self.owners.insert(seen.digest, seen);
        let mut kept = 0;
        while self.owned_order.len() > OWNED && kept < self.owned_order.len() {
            let Some(digest) = self.owned_order.pop_front() else {
                break;
            };
            if self
                .owners
                .get(&digest)
                .is_some_and(|o| o.live(&caller_socket))
            {
                self.owned_order.push_back(digest);
                kept += 1;
            } else {
                self.owners.remove(&digest);
            }
        }
        false
    }

    /// Whether `seen` repeats key material that already admitted a
    /// connection, from anyone.
    pub fn admitted(&self, seen: &Seen) -> bool {
        self.admitted.iter().any(|e| e.seen.digest == seen.digest)
    }

    /// Remembers the key material of a caller just admitted on `path`,
    /// evicting the path's oldest ended entry past [`PER_PATH`] and the
    /// oldest ended entry of all past [`TOTAL`].
    pub fn record(
        &mut self,
        path: &str,
        seen: Seen,
        caller_socket: impl Fn(ConnId) -> Option<u32>,
    ) {
        self.admitted.retain(|e| e.seen.digest != seen.digest);
        let ended = |e: &Entry| !e.seen.live(&caller_socket);
        if self.admitted.iter().filter(|e| &*e.path == path).count() >= PER_PATH
            && let Some(oldest) = self
                .admitted
                .iter()
                .position(|e| &*e.path == path && ended(e))
        {
            self.admitted.remove(oldest);
        }
        if self.admitted.len() >= TOTAL
            && let Some(oldest) = self.admitted.iter().position(ended)
        {
            self.admitted.remove(oldest);
        }
        self.admitted.push_back(Entry {
            path: path.into(),
            seen,
        });
    }

    #[cfg(test)]
    fn sizes(&self) -> (usize, usize) {
        (self.owners.len(), self.admitted.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 1], port))
    }

    /// Distinct key material for `n`.
    fn seen(n: u32, port: u16, socket: u32, conn: u32) -> Seen {
        Seen::new(&n.to_be_bytes(), peer(port), socket, ConnId(conn))
    }

    /// No connection is waiting or open.
    const GONE: fn(ConnId) -> Option<u32> = |_| None;

    /// Only connection `conn` is, with caller socket `socket`.
    fn only(conn: u32, socket: u32) -> impl Fn(ConnId) -> Option<u32> {
        move |c| (c == ConnId(conn)).then_some(socket)
    }

    #[test]
    fn the_first_sender_owns_key_material_and_anyone_else_is_refused() {
        let mut guard = ReplayGuard::default();
        assert!(!guard.connect(seen(1, 5000, 7, 100), GONE), "first sight");
        for (port, socket, what) in [
            (5001, 7, "another address"),
            (5000, 8, "another caller socket"),
            (5001, 8, "both"),
        ] {
            assert!(guard.connect(seen(1, port, socket, 101), GONE), "{what}");
        }
        // The owner itself, on a new connection after a refusal: let through.
        assert!(!guard.connect(seen(1, 5000, 7, 102), GONE), "owner");
        assert!(
            !guard.connect(seen(2, 5001, 8, 103), GONE),
            "other key material"
        );
    }

    #[test]
    fn admitted_key_material_is_refused_to_everyone_its_owner_included() {
        let mut guard = ReplayGuard::default();
        assert!(!guard.connect(seen(1, 5000, 7, 100), GONE));
        assert!(!guard.admitted(&seen(1, 5000, 7, 100)));
        guard.record("live/event-a", seen(1, 5000, 7, 100), GONE);
        assert!(guard.admitted(&seen(1, 5001, 9, 101)));
        assert!(guard.connect(seen(1, 5001, 9, 101), GONE), "another caller");
        assert!(
            guard.connect(seen(1, 5000, 7, 101), GONE),
            "its owner, later"
        );
    }

    #[test]
    fn lookups_span_paths() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-a", seen(1, 5000, 7, 100), GONE);
        // What path the replay names does not matter.
        assert!(guard.admitted(&seen(1, 5001, 8, 101)));
    }

    #[test]
    fn a_path_keeps_its_newest_ended_entries() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-other", seen(0, 4000, 1, 1), GONE);
        let n = u32::try_from(PER_PATH).expect("small");
        for i in 1..=n + 1 {
            guard.record("live/event-a", seen(i, 5000, i, i), GONE);
        }
        assert_eq!(guard.sizes().1, PER_PATH + 1);
        assert!(!guard.admitted(&seen(1, 6000, 0, 0)), "oldest evicted");
        for i in 2..=n + 1 {
            assert!(guard.admitted(&seen(i, 6000, 0, 0)), "{i} kept");
        }
        assert!(guard.admitted(&seen(0, 6000, 0, 0)), "other path kept");
    }

    #[test]
    fn a_live_stage_keeps_its_entry_through_a_flood_of_admissions() {
        let mut guard = ReplayGuard::default();
        let live = only(1, 7);
        guard.record("live/event-a", seen(0, 5000, 7, 1), &live);
        let flood = u32::try_from(TOTAL * 3).expect("small");
        for i in 1..=flood {
            // Its own path too, past its per-path bound.
            let path = if i % 2 == 0 {
                "live/event-a".to_owned()
            } else {
                format!("live/event-{}", i % 97)
            };
            guard.record(&path, seen(i, 6000, i, i + 1), &live);
            assert!(guard.sizes().1 <= TOTAL);
        }
        assert!(guard.admitted(&seen(0, 6001, 9, 9)), "the live entry");
        assert!(!guard.admitted(&seen(2, 6001, 9, 9)), "an old ended one");
    }

    #[test]
    fn a_flood_of_distinct_admissions_stays_within_the_bounds() {
        let mut guard = ReplayGuard::default();
        let flood = u32::try_from(TOTAL * 3).expect("small");
        for i in 0..flood {
            let path = format!("live/event-{}", i % 97);
            guard.record(&path, seen(i, 5000, i, i), GONE);
            assert!(guard.sizes().1 <= TOTAL);
        }
        assert_eq!(guard.sizes().1, TOTAL);
        let per_path = |p: &str| guard.admitted.iter().filter(|e| &*e.path == p).count();
        assert!((0..97).all(|p| per_path(&format!("live/event-{p}")) <= PER_PATH));
        // Oldest first: the last TOTAL admissions are the ones remembered.
        let kept = flood - u32::try_from(TOTAL).expect("small");
        assert!(!guard.admitted(&seen(kept - 1, 6000, 0, 0)));
        assert!(guard.admitted(&seen(kept, 6000, 0, 0)));
        assert!(guard.admitted(&seen(flood - 1, 6000, 0, 0)));
    }

    #[test]
    fn owners_stay_within_their_bound_and_a_waiting_owner_survives_a_flood() {
        let mut guard = ReplayGuard::default();
        let live = only(1, 7);
        assert!(!guard.connect(seen(0, 5000, 7, 1), &live), "waiting owner");
        assert!(!guard.connect(seen(1, 5001, 8, 2), &live), "ended owner");
        let flood = u32::try_from(OWNED * 2).expect("small");
        for i in 2..flood {
            assert!(!guard.connect(seen(i, 6000, i, i + 10), &live));
            assert!(guard.sizes().0 <= OWNED);
        }
        assert!(guard.connect(seen(0, 6001, 9, 9), &live), "still owned");
        assert!(
            !guard.connect(seen(1, 6001, 9, 9), &live),
            "evicted, oldest"
        );
        assert!(
            guard.connect(seen(flood - 1, 6002, 9, 9), &live),
            "newest kept"
        );
        // Admitted entries are a tier of their own: a flood of owners never
        // pushes one out.
        guard.record("live/event-a", seen(u32::MAX, 5000, 7, 1), &live);
        for i in flood..flood + u32::try_from(OWNED).expect("small") {
            guard.connect(seen(i, 6000, i, i + 10), &live);
        }
        assert!(guard.admitted(&seen(u32::MAX, 6003, 9, 9)));
    }

    #[test]
    fn recording_the_same_key_material_again_keeps_one_entry() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-a", seen(1, 5000, 7, 100), GONE);
        guard.record("live/event-a", seen(1, 5000, 7, 101), GONE);
        assert_eq!(guard.sizes().1, 1);
    }

    #[test]
    fn debug_shows_no_key_material() {
        let bound = [0xAB; 40];
        let text = format!("{:?}", Seen::new(&bound, peer(5000), 7, ConnId(1)));
        let digest: [u8; 32] = Sha256::digest(bound).into();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!text.contains("ab") && !text.contains(&hex[..8]), "{text}");
    }
}
