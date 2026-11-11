//! Router tests: routing by USERNAME and by tuple, and tuple ownership
//! against checks that arrive from another session's tuple.

use super::*;
use crate::ice::{Kind, classify};
use crate::session::SessionConfig;
use crate::testing::*;

const GCM: u16 = 0x0007;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

struct Side {
    p: Publisher,
    id: SessionId,
}

fn router() -> Router {
    Router::new(RouterConfig::default()).expect("defaults")
}

/// A publisher with its session in the router.
fn join(r: &mut Router, seed: u8, t0: Instant) -> Side {
    let mut p = Publisher::new(seed, &[GCM]);
    let a = accept(&p.offer(), SessionConfig::default(), t0, seed);
    p.answered(&a);
    let id = SessionId::generate_seeded([seed; 32]).expect("id");
    r.insert(id, a.session).expect("room");
    Side { p, id }
}

/// Hands what the router sent to the publisher at that tuple, and feeds
/// back the DTLS they answer with, until nothing more comes. Events are
/// dropped: these tests count media by the sessions' SRTP counters.
fn pump(r: &mut Router, now: Instant, sides: &mut [&mut Side]) {
    for _ in 0..32 {
        while r.poll_event().is_some() {}
        let mut replies = Vec::new();
        while let Some(t) = r.poll_transmit() {
            // Feedback can reach a publisher that another session sends to
            // after a check from its tuple; these tests do not read it.
            if classify(&t.bytes) == Kind::Rtcp {
                continue;
            }
            if let Some(s) = sides.iter_mut().find(|s| s.p.tuple == t.tuple) {
                let tuple = s.p.tuple;
                replies.extend(s.p.take(t).into_iter().map(|d| (tuple, d)));
            }
        }
        if replies.is_empty() {
            return;
        }
        for (tuple, d) in replies {
            r.handle_datagram(now, tuple, &d);
        }
    }
}

/// Nominates, then runs the DTLS handshake through the router.
fn connect(r: &mut Router, now: Instant, side: &mut Side) {
    let tuple = side.p.tuple;
    let c = side.p.check(true);
    assert_eq!(r.handle_datagram(now, tuple, &c), Some(side.id));
    for d in side.p.client.start() {
        r.handle_datagram(now, tuple, &d);
    }
    pump(r, now, &mut [&mut *side]);
    assert!(side.p.has_keys(), "handshake completes");
    assert_eq!(state(r, side), Some(State::Connected));
}

fn state(r: &Router, side: &Side) -> Option<State> {
    r.session(&side.id).map(Session::state)
}

/// SRTP packets the session authenticated: media that reached it.
fn srtp(r: &Router, side: &Side) -> u64 {
    r.session(&side.id).map_or(0, |s| s.stats().srtp_packets)
}

/// `n` audio packets from the side's own tuple, 20 ms apart from `from`.
/// Returns the time of the last.
fn talk(r: &mut Router, side: &mut Side, from: Instant, n: u64) -> Instant {
    let tuple = side.p.tuple;
    let mut now = from;
    for i in 1..=n {
        now = from + ms(20 * i);
        let a = side.p.audio(i as u8);
        r.handle_datagram(now, tuple, &a);
        r.handle_timeout(now);
        pump(r, now, &mut [&mut *side]);
    }
    now
}

/// What happened to Y's media when a valid check for X arrived from Y's
/// tuple in the middle of it.
struct Outcome {
    sent: u64,
    reached_y: u64,
    owner_is_y: bool,
    x_validated: bool,
    x_refused: u64,
}

fn check_from_a_media_tuple(use_candidate: bool) -> Outcome {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 1, t0);
    let mut x = join(&mut r, 2, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 50);
    // X's publisher's check, copied or spoofed from Y's tuple.
    let c = x.p.check(use_candidate);
    assert_eq!(
        r.handle_datagram(now, ty, &c),
        Some(x.id),
        "routed by USERNAME"
    );
    let x_validated = r.session(&x.id).expect("X").accepts(&ty);
    let refused_before = r.session(&x.id).expect("X").stats().srtp_refused;
    talk(&mut r, &mut y, now, 50);
    Outcome {
        sent: 100,
        reached_y: srtp(&r, &y),
        owner_is_y: r.owner(&ty) == Some(y.id),
        x_validated,
        x_refused: r.session(&x.id).expect("X").stats().srtp_refused - refused_before,
    }
}

/// Required for W3c: a check for session X that arrives from the tuple
/// session Y is delivering fresh media on must not cost Y its media. "The
/// session whose check from it came last" fails this: every packet Y sent
/// after the check went to X, which refused them.
#[test]
fn a_check_for_another_session_cannot_take_a_media_tuple() {
    for use_candidate in [false, true] {
        let o = check_from_a_media_tuple(use_candidate);
        println!(
            "USE-CANDIDATE {use_candidate}: {} of {} of Y's packets reached Y, X refused {}",
            o.reached_y, o.sent, o.x_refused
        );
        assert!(o.x_validated, "the check was valid for X");
        assert_eq!(o.reached_y, o.sent, "Y lost media");
        assert_eq!(o.x_refused, 0, "Y's media went to X");
        assert!(o.owner_is_y);
    }
}

/// The first session to validate a tuple keeps it through its handshake:
/// X's valid check, and X's SRTP, from Y's tuple before Y has connected
/// neither take the tuple nor stop Y's handshake.
#[test]
fn the_first_session_keeps_a_tuple_through_its_handshake() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 3, t0);
    let mut x = join(&mut r, 4, t0);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let c = y.p.check(true);
    r.handle_datagram(t0, ty, &c);
    assert_eq!(r.owner(&ty), Some(y.id));
    let kept = r.stats().kept;
    r.handle_datagram(t0, ty, &x.p.check(true));
    r.handle_datagram(t0, ty, &x.p.audio(1));
    assert_eq!(r.stats().kept, kept + 1);
    assert_eq!(r.owner(&ty), Some(y.id));
    for d in y.p.client.start() {
        r.handle_datagram(t0, ty, &d);
    }
    pump(&mut r, t0, &mut [&mut y, &mut x]);
    assert_eq!(state(&r, &y), Some(State::Connected));
    talk(&mut r, &mut y, t0, 10);
    assert_eq!(srtp(&r, &y), 10);
}

/// What the rule leaves open: an admitted publisher whose check from a
/// victim's tuple arrives before the victim's first check, and which then
/// sends its own SRTP from that tuple, keeps it, and the victim cannot
/// connect on it. Once that SRTP stops for the hold, the victim's next
/// check takes the tuple and it connects.
#[test]
fn a_check_that_beats_the_first_one_owns_the_tuple() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 5, t0);
    let mut x = join(&mut r, 6, t0);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    r.handle_datagram(t0, ty, &x.p.check(false));
    r.handle_datagram(t0, ty, &x.p.audio(1));
    assert_eq!(r.owner(&ty), Some(x.id));
    r.handle_datagram(t0, ty, &y.p.check(true));
    assert_eq!(r.owner(&ty), Some(x.id), "X has fresh media from it");
    let hello = y.p.client.start();
    for d in &hello {
        r.handle_datagram(t0, ty, d);
    }
    pump(&mut r, t0, &mut [&mut y, &mut x]);
    assert!(!y.p.has_keys(), "Y's DTLS went to X");
    // X stops; Y's next check after the hold takes the tuple, and Y's
    // client sends its first flight again.
    let later = t0 + ms(2_100);
    r.handle_datagram(later, ty, &y.p.check(true));
    assert_eq!(r.owner(&ty), Some(y.id));
    for d in &hello {
        r.handle_datagram(later, ty, d);
    }
    pump(&mut r, later, &mut [&mut y, &mut x]);
    assert_eq!(state(&r, &y), Some(State::Connected));
}

/// A NAT that gives a silent publisher's port to a new one: once the old
/// session has had no media from the tuple for the hold, the new
/// session's check takes it and its media flows.
#[test]
fn a_silent_tuple_passes_to_the_session_that_checks_from_it() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 7, t0);
    let mut x = join(&mut r, 8, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 20);
    // Y's publisher goes; X's comes back through the same NAT port.
    let later = now + ms(2_100);
    x.p.tuple = ty;
    let moved = r.stats().moved;
    r.handle_datagram(later, ty, &x.p.check(true));
    assert_eq!(r.owner(&ty), Some(x.id));
    assert_eq!(r.stats().moved, moved + 1);
    let before = srtp(&r, &x);
    talk(&mut r, &mut x, later, 10);
    assert_eq!(srtp(&r, &x) - before, 10);
}

/// A publisher silent for longer than the hold can lose its tuple to one
/// spoofed check for another session. Its media is then dropped only until
/// its own next check, which takes the tuple back: the other session has
/// no fresh media from it.
#[test]
fn a_publisher_back_after_a_gap_reclaims_its_tuple_at_its_next_check() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 9, t0);
    let mut x = join(&mut r, 10, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 20);
    let gap = now + ms(2_100);
    r.handle_datagram(gap, ty, &x.p.check(false));
    assert_eq!(r.owner(&ty), Some(x.id));
    // Y resumes: these go to X, which cannot authenticate them.
    let before = srtp(&r, &y);
    let resumed = talk(&mut r, &mut y, gap, 5);
    assert_eq!(srtp(&r, &y), before, "while X owned the tuple");
    r.handle_datagram(resumed, ty, &y.p.check(false));
    assert_eq!(r.owner(&ty), Some(y.id));
    talk(&mut r, &mut y, resumed, 5);
    assert_eq!(srtp(&r, &y), before + 5);
}

/// Limit one when the session that takes a silent publisher's tuple has
/// not connected: it keeps the tuple while it handshakes, so the
/// publisher's own check does not take it back, and its media is dropped
/// until that session ends at its handshake deadline (10 s after it was
/// made). The publisher's next check after that takes the tuple, and its
/// media flows again.
#[test]
fn an_unconnected_session_keeps_a_taken_tuple_until_its_deadline() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 20, t0);
    let mut x = join(&mut r, 21, t0);
    connect(&mut r, t0, &mut y);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 20);
    let gap = now + ms(2_100);
    r.handle_datagram(gap, ty, &x.p.check(false));
    assert_eq!(r.owner(&ty), Some(x.id));
    assert_eq!(state(&r, &x), Some(State::Checking));
    // Y resumes: its media goes to X, which has no keys.
    let before = srtp(&r, &y);
    let resumed = talk(&mut r, &mut y, gap, 10);
    assert_eq!(srtp(&r, &y), before, "dropped while X owns the tuple");
    // Y's own check does not take the tuple back from a session that has
    // not connected.
    let kept = r.stats().kept;
    r.handle_datagram(resumed, ty, &y.p.check(false));
    assert_eq!(r.owner(&ty), Some(x.id));
    assert_eq!(r.stats().kept, kept + 1);
    // X ends at its handshake deadline and takes its tuples with it; Y's
    // media from the tuple is then routed nowhere until Y's next check.
    let deadline = t0 + Duration::from_secs(10);
    r.handle_timeout(deadline);
    assert!(r.session(&x.id).is_none(), "X ended at its deadline");
    assert_eq!(r.owner(&ty), None);
    let a = y.p.audio(1);
    assert_eq!(r.handle_datagram(deadline, ty, &a), None);
    assert_eq!(srtp(&r, &y), before);
    r.handle_datagram(deadline, ty, &y.p.check(false));
    assert_eq!(r.owner(&ty), Some(y.id));
    talk(&mut r, &mut y, deadline, 5);
    assert_eq!(srtp(&r, &y), before + 5);
}

/// Limit two after a silence: an admitted publisher that spoofs its own
/// SRTP from a silent publisher's tuple keeps it, and the publisher's
/// checks and media from it do not get it back.
#[test]
fn spoofed_media_keeps_a_tuple_taken_after_a_silence() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 22, t0);
    let mut x = join(&mut r, 23, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 20);
    let gap = now + ms(2_100);
    r.handle_datagram(gap, ty, &x.p.check(false));
    r.handle_datagram(gap, ty, &x.p.audio(1));
    assert_eq!(r.owner(&ty), Some(x.id));
    let before = srtp(&r, &y);
    for i in 1..=10u64 {
        let at = gap + ms(20 * i);
        r.handle_datagram(at, ty, &x.p.audio(i as u8 + 1));
        r.handle_datagram(at, ty, &y.p.check(false));
        r.handle_datagram(at, ty, &y.p.audio(i as u8));
        pump(&mut r, at, &mut [&mut y, &mut x]);
    }
    assert_eq!(r.owner(&ty), Some(x.id));
    assert_eq!(srtp(&r, &y), before, "all of Y's media went to X");
}

/// Only a check the session takes as new moves a tuple: neither a
/// retransmission of a remembered ID nor a request that fails
/// authentication does, though the session has the tuple validated.
#[test]
fn a_replayed_or_forged_check_moves_nothing() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 11, t0);
    let mut x = join(&mut r, 12, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut y, t0, 20);
    // X validates Y's tuple during a gap, then Y takes it back.
    let gap = now + ms(2_100);
    let c = x.p.check(false);
    r.handle_datagram(gap, ty, &c);
    assert_eq!(r.owner(&ty), Some(x.id));
    r.handle_datagram(gap, ty, &y.p.check(false));
    assert_eq!(r.owner(&ty), Some(y.id));
    // Another gap: the same check again, and a forged one, from Y's tuple.
    let later = gap + ms(2_100);
    let claims = r.stats().claims;
    r.handle_datagram(later, ty, &c);
    let mut forged = x.p.check(false);
    let last = forged.len() - 20;
    forged[last] ^= 1;
    r.handle_datagram(later, ty, &forged);
    assert!(r.session(&x.id).expect("X").accepts(&ty));
    assert_eq!(r.owner(&ty), Some(y.id));
    assert_eq!(r.stats().claims, claims);
}

/// A request naming no live session, other STUN, and anything from a
/// tuple no session owns draw nothing.
#[test]
fn unroutable_datagrams_draw_nothing() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 13, t0);
    let mut stranger = StunClient::new("nobody00", "nobody-password-000000000", "them");
    let stray = tuple(9);
    assert_eq!(r.handle_datagram(t0, stray, &stranger.check(true)), None);
    assert_eq!(
        r.handle_datagram(t0, stray, &[0x16, 0xfe, 0xfd, 0, 0]),
        None
    );
    let rtp = [0x80, 0x6f, 0, 1, 0, 0, 0, 0, 0, 0, 0x10, 0x01];
    assert_eq!(r.handle_datagram(t0, stray, &rtp), None);
    assert_eq!(r.handle_datagram(t0, stray, &[0x01, 0x01, 0, 0]), None);
    assert!(r.poll_transmit().is_none());
    assert_eq!(r.stats().unrouted, 4);
    // A check for Y from a fresh tuple is Y's, and only its answer goes out.
    assert_eq!(r.handle_datagram(t0, stray, &y.p.check(false)), Some(y.id));
    let answers: Vec<Transmit> = std::iter::from_fn(|| r.poll_transmit()).collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].tuple, stray);
}

/// Sessions are refused past the cap, and for a repeated ID or ufrag.
#[test]
fn insert_refuses_past_the_cap_and_duplicates() {
    let t0 = Instant::now();
    let mut r = Router::new(RouterConfig {
        max_sessions: 2,
        ..RouterConfig::default()
    })
    .expect("two");
    let a = join(&mut r, 14, t0);
    let p = Publisher::new(15, &[GCM]);
    let dup_ufrag = accept(&p.offer(), SessionConfig::default(), t0, 14).session;
    let other_id = SessionId::generate_seeded([99; 32]).expect("id");
    assert_eq!(
        r.insert(other_id, dup_ufrag),
        Err(InsertError::DuplicateUfrag)
    );
    let fresh = accept(&p.offer(), SessionConfig::default(), t0, 15).session;
    assert_eq!(r.insert(a.id, fresh), Err(InsertError::DuplicateId));
    let _b = join(&mut r, 16, t0);
    let third = accept(&p.offer(), SessionConfig::default(), t0, 17).session;
    assert_eq!(r.insert(other_id, third), Err(InsertError::Full));
    assert_eq!(r.len(), 2);
    assert!(
        Router::new(RouterConfig {
            max_sessions: 0,
            ..RouterConfig::default()
        })
        .is_err()
    );
}

/// A closed or ended session is removed, with its ufrag and its tuples,
/// after its last event.
#[test]
fn an_ended_session_is_removed_after_its_last_event() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 18, t0);
    connect(&mut r, t0, &mut y);
    let ty = y.p.tuple;
    assert!(r.close(&y.id, t0));
    let events: Vec<(SessionId, Event)> = std::iter::from_fn(|| r.poll_event()).collect();
    assert!(matches!(events.last(), Some((id, Event::Ended(_))) if *id == y.id));
    assert!(r.session(&y.id).is_none());
    assert_eq!(r.owner(&ty), None);
    assert_eq!(r.handle_datagram(t0, ty, &y.p.check(false)), None);
    assert_eq!(r.stats().removed, 1);
    assert!(r.is_empty());
    // A handshake that never completes ends at its deadline and goes too.
    let z = join(&mut r, 19, t0);
    let deadline = r.poll_timeout().expect("the handshake deadline");
    r.handle_timeout(deadline);
    assert!(r.session(&z.id).is_none());
}

/// A panic in one session's call drops that session only: it ends with
/// `Panicked`, its ufrag and tuple are freed, and the other session's media
/// goes on without a packet lost.
#[test]
fn a_panicking_session_is_dropped_and_the_others_go_on() {
    let t0 = Instant::now();
    let mut r = router();
    let mut y = join(&mut r, 24, t0);
    let mut x = join(&mut r, 25, t0);
    connect(&mut r, t0, &mut y);
    connect(&mut r, t0, &mut x);
    let ty = y.p.tuple;
    let now = talk(&mut r, &mut x, t0, 10);
    while r.poll_event().is_some() {}
    assert_eq!(r.handle_datagram(now, ty, PANIC_DATAGRAM), Some(y.id));
    let events: Vec<(SessionId, Event)> = std::iter::from_fn(|| r.poll_event()).collect();
    assert!(matches!(
        events.as_slice(),
        [(id, Event::Ended(crate::session::EndReason::Panicked))] if *id == y.id
    ));
    assert!(r.session(&y.id).is_none());
    assert_eq!(r.owner(&ty), None);
    assert_eq!(r.stats().panics, 1);
    assert_eq!(
        r.handle_datagram(now, ty, &y.p.check(false)),
        None,
        "its ufrag is free"
    );
    // The other session took no harm.
    let before = srtp(&r, &x);
    talk(&mut r, &mut x, now, 10);
    assert_eq!(srtp(&r, &x), before + 10);
    assert_eq!(state(&r, &x), Some(State::Connected));
}
