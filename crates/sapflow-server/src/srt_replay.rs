//! Refuses SRT key material that already admitted another connection.
//!
//! A passing key check proves only that a caller's key material (its
//! `KMREQ`: salt and wrapped keys, sent in clear) was wrapped under the
//! passphrase, not who sent it. Anyone who captured an encoder's conclusion
//! handshake can replay it from another address and pass the check, and an
//! SRT claim takes over a live path: they cannot decrypt or forge media, but
//! they knock the real encoder off. Real encoders make fresh key material
//! for every connection (our `Caller`, libsrt and OBS, checked), so key
//! material seen before on an admitted connection is a replay.
//!
//! The guard keeps, in memory, a SHA-256 digest of the part of each admitted
//! caller's key material that the passphrase check binds
//! ([`sapflow_srt::crypto::bound_key_material`]): editing anything outside
//! it still passes the check, so it is no way round the guard. Lookups span
//! every path, so key material admitted on one stage is refused on another
//! too (under one deployment-wide passphrase it would unwrap there).
//! Bounds: [`PER_PATH`] entries a path and [`TOTAL`] in all, oldest evicted
//! first. There is no time limit: entries only leave by those bounds or a
//! restart, either of which lets an older capture through again.
//!
//! A conclusion the same caller resends (same address and caller socket id)
//! while its connection waits or is open never reaches the guard: the
//! listener answers it on that connection. Should one arrive as a new
//! connection while the recorded one is still waiting or open, it is let
//! through; after that connection ended it is a replay like any other.

use std::collections::VecDeque;
use std::net::SocketAddr;

use sapflow_srt::ConnId;
use sha2::{Digest, Sha256};

/// Admitted callers' key material remembered per stage path: room for far
/// more reconnects than a show sees, each one an admission with the stage's
/// passphrase.
pub const PER_PATH: usize = 128;
/// All paths together: under a megabyte at most (an entry is its digest,
/// the path, at most 133 bytes, and the caller's address and ids).
pub const TOTAL: usize = 4096;

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

/// Key material of admitted SRT callers, owned by the SRT loop.
#[derive(Default)]
pub struct ReplayGuard {
    /// Oldest first.
    entries: VecDeque<Entry>,
}

impl ReplayGuard {
    /// Whether `seen` repeats key material that admitted an earlier
    /// connection. `caller_socket` says, for a connection, the caller socket
    /// id it has while waiting or open (`Listener::caller_socket`): only the
    /// same caller, on a connection still waiting or open, may repeat it.
    pub fn is_replay(&self, seen: &Seen, caller_socket: impl Fn(ConnId) -> Option<u32>) -> bool {
        self.entries
            .iter()
            .find(|e| e.seen.digest == seen.digest)
            .is_some_and(|e| {
                let live = caller_socket(e.seen.conn) == Some(e.seen.caller_socket);
                !(live && e.seen.same_caller(seen))
            })
    }

    /// Remembers the key material of a caller just admitted on `path`,
    /// evicting the path's oldest entry past [`PER_PATH`] and the oldest of
    /// all past [`TOTAL`].
    pub fn record(&mut self, path: &str, seen: Seen) {
        self.entries.retain(|e| e.seen.digest != seen.digest);
        if self.entries.iter().filter(|e| &*e.path == path).count() >= PER_PATH
            && let Some(oldest) = self.entries.iter().position(|e| &*e.path == path)
        {
            self.entries.remove(oldest);
        }
        while self.entries.len() >= TOTAL {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            path: path.into(),
            seen,
        });
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
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

    const GONE: fn(ConnId) -> Option<u32> = |_| None;

    #[test]
    fn admitted_key_material_is_refused_from_anyone_else() {
        let mut guard = ReplayGuard::default();
        assert!(
            !guard.is_replay(&seen(1, 5000, 7, 100), GONE),
            "first sight"
        );
        guard.record("live/event-a", seen(1, 5000, 7, 100));
        let open = |c: ConnId| (c == ConnId(100)).then_some(7);
        for (port, socket, what) in [
            (5001, 7, "another address"),
            (5000, 8, "another caller socket"),
            (5001, 8, "both"),
        ] {
            assert!(guard.is_replay(&seen(1, port, socket, 101), open), "{what}");
        }
        assert!(
            !guard.is_replay(&seen(2, 5001, 7, 101), open),
            "other key material"
        );
    }

    #[test]
    fn the_same_caller_may_repeat_only_while_its_connection_waits_or_is_open() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-a", seen(1, 5000, 7, 100));
        let open = |c: ConnId| (c == ConnId(100)).then_some(7);
        assert!(!guard.is_replay(&seen(1, 5000, 7, 101), open), "still open");
        // Ended: the same address and caller socket are refused like anyone.
        assert!(guard.is_replay(&seen(1, 5000, 7, 101), GONE), "ended");
        // A different connection now holding that id is not this caller's.
        let reused = |c: ConnId| (c == ConnId(100)).then_some(9);
        assert!(guard.is_replay(&seen(1, 5000, 7, 101), reused));
    }

    #[test]
    fn lookups_span_paths() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-a", seen(1, 5000, 7, 100));
        // What path the replay names does not matter.
        assert!(guard.is_replay(&seen(1, 5001, 8, 101), GONE));
    }

    #[test]
    fn a_path_keeps_its_newest_entries() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-other", seen(0, 4000, 1, 1));
        let n = u32::try_from(PER_PATH).expect("small");
        for i in 1..=n + 1 {
            guard.record("live/event-a", seen(i, 5000, i, i));
        }
        assert_eq!(guard.len(), PER_PATH + 1);
        assert!(
            !guard.is_replay(&seen(1, 6000, 0, 0), GONE),
            "oldest evicted"
        );
        for i in 2..=n + 1 {
            assert!(guard.is_replay(&seen(i, 6000, 0, 0), GONE), "{i} kept");
        }
        assert!(
            guard.is_replay(&seen(0, 6000, 0, 0), GONE),
            "other path kept"
        );
    }

    #[test]
    fn a_flood_of_distinct_key_material_stays_within_the_bounds() {
        let mut guard = ReplayGuard::default();
        let flood = u32::try_from(TOTAL * 3).expect("small");
        for i in 0..flood {
            let path = format!("live/event-{}", i % 97);
            guard.record(&path, seen(i, 5000, i, i));
            assert!(guard.len() <= TOTAL);
        }
        assert_eq!(guard.len(), TOTAL);
        let per_path = |p: &str| guard.entries.iter().filter(|e| &*e.path == p).count();
        assert!((0..97).all(|p| per_path(&format!("live/event-{p}")) <= PER_PATH));
        // Oldest first: the last TOTAL admissions are the ones remembered.
        let kept = flood - u32::try_from(TOTAL).expect("small");
        assert!(!guard.is_replay(&seen(kept - 1, 6000, 0, 0), GONE));
        assert!(guard.is_replay(&seen(kept, 6000, 0, 0), GONE));
        assert!(guard.is_replay(&seen(flood - 1, 6000, 0, 0), GONE));
    }

    #[test]
    fn recording_the_same_key_material_again_keeps_one_entry() {
        let mut guard = ReplayGuard::default();
        guard.record("live/event-a", seen(1, 5000, 7, 100));
        guard.record("live/event-a", seen(1, 5000, 7, 101));
        assert_eq!(guard.len(), 1);
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
