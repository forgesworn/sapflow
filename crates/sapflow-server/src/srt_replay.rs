//! Refuses SRT key material that belongs to another caller.
//!
//! A passing key check proves only that a caller's key material (its
//! `KMREQ`: salt and wrapped keys, sent in clear) was wrapped under the
//! passphrase, not who sent it. Anyone who captured an encoder's conclusion
//! handshake can replay it from another address and pass the check, and an
//! SRT claim takes over a live path. A copy of an attempt that never carried
//! media gives its sender nothing to decrypt or send, but it would knock the
//! real encoder off; a copy of an admitted connection's handshake also
//! restores that connection's keys, so whoever recorded the connection could
//! send its media again and it would decrypt onto the stage (and AES-CTR,
//! unauthenticated, lets predictable bytes be flipped). Real encoders make fresh key material for every connection
//! (our `Caller`, libsrt and OBS, checked), so key material seen before from
//! another caller is a replay.
//!
//! The guard compares a SHA-256 digest of the part of the key material that
//! the passphrase check binds ([`sapflow_srt::crypto::bound_key_material`]):
//! editing anything outside it still passes the check, so it is no way round
//! the guard. It keeps two tiers, and lookups span every path:
//!
//! - Owners, in memory: when a keyed caller first connects, before the
//!   throttle or the app is consulted, its key material is owned by that
//!   caller (its address and caller socket id). The same key material from
//!   anyone else is refused at once, so a copy that arrives while the
//!   original waits for the app, or after the original was refused for any
//!   reason (throttled, the app's 401, a limit), never gets further. The
//!   owner itself may send it again: a resend after a lost rejection comes
//!   from the same address and socket. [`OWNED`] entries, oldest ended first.
//! - Admitted, in memory and in the store (`SAPFLOW_SRT_REPLAY_STORE`,
//!   [`crate::srt_replay_store`]), written before the path is claimed: key
//!   material of a caller about to be accepted, refused to anyone from then
//!   on, its owner included (while its connection is open the listener
//!   answers the owner's resends on it, so a new connection with it is a
//!   copy), and across restarts. [`PER_PATH`] entries a path and [`TOTAL`] in
//!   all, oldest ended first, so a flood of admissions elsewhere cannot push
//!   out a live stage's entry.
//!
//! An entry whose connection is still waiting or open is never evicted, so
//! either tier may exceed its bound by the connections the listener holds.
//! There is no time limit. Owners leave by their bound or a restart;
//! admitted entries by their bounds, or if the operator discards the store.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;

use sapflow_srt::ConnId;
use sha2::{Digest, Sha256};

use crate::srt_replay_store::{Record, ReplayStore, ReplayStoreError, encode};

/// Admitted callers' key material remembered per stage path: room for far
/// more reconnects than a show sees, each one an admission with the stage's
/// passphrase.
pub const PER_PATH: usize = 128;
/// Admitted entries on all paths together: under a megabyte in memory, and
/// at most about 680 KB on disk (an entry is its digest and its path, at
/// most 133 bytes; typically about 60 bytes).
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

#[cfg(test)]
impl Seen {
    fn digest_for_test(&self) -> &[u8; 32] {
        &self.digest
    }
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seen").finish_non_exhaustive()
    }
}

struct Entry {
    path: Box<str>,
    digest: [u8; 32],
    /// The connection and its caller socket id; `None` for an entry read
    /// from the store, whose connection ended before the restart.
    conn: Option<(ConnId, u32)>,
}

impl Entry {
    fn ended(&self, caller_socket: &impl Fn(ConnId) -> Option<u32>) -> bool {
        self.conn
            .is_none_or(|(conn, socket)| caller_socket(conn) != Some(socket))
    }
}

/// Key material of SRT callers, kept for the life of the process (across
/// SRT listener restarts) and owned by the SRT loop.
#[derive(Default)]
pub struct ReplayGuard {
    owners: HashMap<[u8; 32], Seen>,
    /// The owners' digests, oldest first.
    owned_order: VecDeque<[u8; 32]>,
    /// Oldest first.
    admitted: VecDeque<Entry>,
    /// The admitted digests, for lookups.
    admitted_index: HashSet<[u8; 32]>,
    store: Option<ReplayStore>,
}

impl ReplayGuard {
    /// A guard whose admitted tier is kept in `store`, starting from what
    /// it held (`records`, oldest first), within the bounds.
    pub fn with_store(store: ReplayStore, records: Vec<Record>) -> Self {
        let mut guard = Self::default();
        for (path, digest) in records {
            guard.insert(&path, digest, None, &|_| None);
        }
        guard.store = Some(store);
        guard
    }

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
        self.admitted_index.contains(&seen.digest)
    }

    /// Remembers the key material of a caller about to be admitted on
    /// `path`, evicting the path's oldest ended entry past [`PER_PATH`] and
    /// the oldest ended entry of all past [`TOTAL`], then writes the
    /// admitted tier to the store, if there is one. On an error the entry is
    /// kept in memory, but a restart would forget it: do not admit the
    /// caller.
    pub fn record(
        &mut self,
        path: &str,
        seen: Seen,
        caller_socket: impl Fn(ConnId) -> Option<u32>,
    ) -> Result<(), ReplayStoreError> {
        self.insert(
            path,
            seen.digest,
            Some((seen.conn, seen.caller_socket)),
            &caller_socket,
        );
        let Some(store) = &mut self.store else {
            return Ok(());
        };
        let bytes = encode(self.admitted.iter().map(|e| (&*e.path, &e.digest)))?;
        store.write(&bytes)
    }

    fn insert(
        &mut self,
        path: &str,
        digest: [u8; 32],
        conn: Option<(ConnId, u32)>,
        caller_socket: &impl Fn(ConnId) -> Option<u32>,
    ) {
        if self.admitted_index.contains(&digest) {
            self.admitted.retain(|e| e.digest != digest);
        }
        if self.admitted.iter().filter(|e| &*e.path == path).count() >= PER_PATH
            && let Some(oldest) = self
                .admitted
                .iter()
                .position(|e| &*e.path == path && e.ended(caller_socket))
        {
            self.evict(oldest);
        }
        if self.admitted.len() >= TOTAL
            && let Some(oldest) = self.admitted.iter().position(|e| e.ended(caller_socket))
        {
            self.evict(oldest);
        }
        self.admitted_index.insert(digest);
        self.admitted.push_back(Entry {
            path: path.into(),
            digest,
            conn,
        });
    }

    fn evict(&mut self, at: usize) {
        if let Some(entry) = self.admitted.remove(at) {
            self.admitted_index.remove(&entry.digest);
        }
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
        guard
            .record("live/event-a", seen(1, 5000, 7, 100), GONE)
            .expect("recorded");
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
        guard
            .record("live/event-a", seen(1, 5000, 7, 100), GONE)
            .expect("recorded");
        // What path the replay names does not matter.
        assert!(guard.admitted(&seen(1, 5001, 8, 101)));
    }

    #[test]
    fn a_path_keeps_its_newest_ended_entries() {
        let mut guard = ReplayGuard::default();
        guard
            .record("live/event-other", seen(0, 4000, 1, 1), GONE)
            .expect("recorded");
        let n = u32::try_from(PER_PATH).expect("small");
        for i in 1..=n + 1 {
            guard
                .record("live/event-a", seen(i, 5000, i, i), GONE)
                .expect("recorded");
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
        guard
            .record("live/event-a", seen(0, 5000, 7, 1), &live)
            .expect("recorded");
        let flood = u32::try_from(TOTAL * 3).expect("small");
        for i in 1..=flood {
            // Its own path too, past its per-path bound.
            let path = if i % 2 == 0 {
                "live/event-a".to_owned()
            } else {
                format!("live/event-{}", i % 97)
            };
            guard
                .record(&path, seen(i, 6000, i, i + 1), &live)
                .expect("recorded");
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
            guard
                .record(&path, seen(i, 5000, i, i), GONE)
                .expect("recorded");
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
        guard
            .record("live/event-a", seen(u32::MAX, 5000, 7, 1), &live)
            .expect("recorded");
        for i in flood..flood + u32::try_from(OWNED).expect("small") {
            guard.connect(seen(i, 6000, i, i + 10), &live);
        }
        assert!(guard.admitted(&seen(u32::MAX, 6003, 9, 9)));
    }

    #[test]
    fn the_store_keeps_admitted_key_material_across_a_restart() {
        use crate::srt_replay_store::tests::Scratch;
        let scratch = Scratch::new();
        let (store, records) = ReplayStore::open(&scratch.path()).expect("first start");
        let mut guard = ReplayGuard::with_store(store, records);
        assert!(!guard.connect(seen(1, 5000, 7, 100), GONE));
        guard
            .record("live/event-a", seen(1, 5000, 7, 100), only(100, 7))
            .expect("recorded and written");
        drop(guard);
        // A restart: no owners, no connections, but the admission is known.
        let (store, records) = ReplayStore::open(&scratch.path()).expect("restart");
        let mut guard = ReplayGuard::with_store(store, records);
        assert!(guard.admitted(&seen(1, 6000, 9, 1)));
        assert!(
            guard.connect(seen(1, 6000, 9, 1), GONE),
            "refused at first sight"
        );
        assert!(guard.connect(seen(1, 5000, 7, 1), GONE), "its owner too");
        assert!(
            !guard.connect(seen(2, 6000, 9, 2), GONE),
            "other key material"
        );
    }

    #[test]
    fn a_store_read_at_start_is_held_to_the_bounds_newest_kept() {
        use crate::srt_replay_store::tests::Scratch;
        let scratch = Scratch::new();
        let (mut store, _) = ReplayStore::open(&scratch.path()).expect("first start");
        let n = u32::try_from(PER_PATH).expect("small") + 5;
        let records: Vec<Record> = (0..n)
            .map(|i| {
                (
                    "live/event-a".to_owned(),
                    *seen(i, 1, 1, 1).digest_for_test(),
                )
            })
            .collect();
        store
            .write(&encode(records.iter().map(|(p, d)| (p.as_str(), d))).expect("encode"))
            .expect("write");
        drop(store);
        let (store, records) = ReplayStore::open(&scratch.path()).expect("restart");
        let guard = ReplayGuard::with_store(store, records);
        assert_eq!(guard.sizes().1, PER_PATH);
        assert!(!guard.admitted(&seen(4, 6000, 0, 0)), "oldest dropped");
        assert!(guard.admitted(&seen(5, 6000, 0, 0)) && guard.admitted(&seen(n - 1, 6000, 0, 0)));
    }

    #[test]
    fn a_failed_store_write_is_reported_and_the_entry_still_refused_in_memory() {
        use crate::srt_replay_store::tests::Scratch;
        let scratch = Scratch::new();
        let (mut store, records) = ReplayStore::open(&scratch.path()).expect("first start");
        store.fail_before_rename = true;
        let mut guard = ReplayGuard::with_store(store, records);
        assert!(
            guard
                .record("live/event-a", seen(1, 5000, 7, 100), GONE)
                .is_err()
        );
        assert!(guard.admitted(&seen(1, 6000, 9, 1)));
    }

    #[test]
    fn recording_the_same_key_material_again_keeps_one_entry() {
        let mut guard = ReplayGuard::default();
        guard
            .record("live/event-a", seen(1, 5000, 7, 100), GONE)
            .expect("recorded");
        guard
            .record("live/event-a", seen(1, 5000, 7, 101), GONE)
            .expect("recorded");
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
