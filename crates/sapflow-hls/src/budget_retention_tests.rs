//! Retention for RFC 8216bis 6.2.2 against the shared fragment budget: what
//! the muxer keeps for a part or segment that has left the window must
//! never cause any live stream's new media to be refused, its own or
//! another's on the same budget.
use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use sapflow_core::fragment::FragmentQuota;

use crate::muxer::tests::{aac, video};
use crate::{Muxer, MuxerConfig, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// One steady stream: 30 fps video of `frame_bytes` with a keyframe each
/// second, and 48 kHz AAC.
struct Steady {
    m: Muxer,
    frame: i64,
    next_audio: i64,
    frame_bytes: usize,
    checker: playlist::RetentionCheck,
}

impl Steady {
    fn new(cfg: MuxerConfig, quota: &Arc<FragmentQuota>, frame_bytes: usize) -> Self {
        Self {
            m: Muxer::with_fragment_quota(cfg, Some(video()), Some(aac()), quota.clone())
                .expect("muxer"),
            frame: 0,
            next_audio: 0,
            frame_bytes,
            checker: playlist::RetentionCheck::default(),
        }
    }

    /// Pushes the next frame and its audio; the error if any push was
    /// refused.
    fn push(&mut self) -> Result<(), crate::muxer::MuxError> {
        let dts = self.frame * 3000;
        while self.next_audio <= dts {
            self.m
                .push_audio(self.next_audio, Bytes::from_static(b"aac"))?;
            self.next_audio += 1920;
        }
        self.m.push_video(
            dts,
            dts,
            self.frame % 30 == 0,
            Bytes::from(vec![0u8; self.frame_bytes]),
            NOW,
        )?;
        self.frame += 1;
        self.checker
            .check(&self.m)
            .expect("a part or segment goes early only when counted");
        Ok(())
    }
}

/// The review's case: two 600 kB/s streams at `segment_count` 7 on one
/// 10 MB budget, A starting 30 s after B, both running until 90 s.
/// Returns the first refusal (stream, its frame) and the peak budget use.
fn two_streams() -> (Option<(char, i64)>, usize, Steady, Steady) {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(10_000_000));
    let mut b = Steady::new(cfg, &quota, 20_000);
    let mut a = Steady::new(cfg, &quota, 20_000);
    let (mut refused, mut peak) = (None, 0);
    for i in 0..90 * 30 {
        if refused.is_none() && b.push().is_err() {
            refused = Some(('B', b.frame));
        }
        if i >= 30 * 30 && refused.is_none() && a.push().is_err() {
            refused = Some(('A', a.frame));
        }
        peak = peak.max(quota.used());
    }
    (refused, peak, a, b)
}

#[test]
fn kept_media_never_fences_another_stream_on_the_same_budget() {
    let (refused, peak, a, b) = two_streams();
    println!(
        "refused {refused:?}, peak budget {peak} bytes; A early parts {} segments {}, \
         B early parts {} segments {}",
        a.m.parts_released_early(),
        a.m.segments_released_early(),
        b.m.parts_released_early(),
        b.m.segments_released_early()
    );
    assert_eq!(refused, None, "a live push was refused");
    // Neither stream's retention had to give way.
    assert_eq!(
        (a.m.parts_released_early(), b.m.parts_released_early()),
        (0, 0)
    );
    assert!(kept(&b.m) > 0, "B kept media past its window");
    let pool = b.m.retention_pool().cloned().expect("a pool");
    assert!(Arc::ptr_eq(&pool, a.m.retention_pool().expect("a pool")));
    let quota = b.m.fragment_quota().expect("a budget");
    drop((a, b));
    assert_eq!((quota.used(), pool.used()), (0, 0), "nothing leaks");
}

/// Parts still served for segments that have left the window.
fn kept(m: &Muxer) -> usize {
    let first = m
        .entries()
        .iter()
        .find_map(|e| match e {
            crate::muxer::Entry::Segment(s) => s.parts.first().map(|p| p.id),
            crate::muxer::Entry::Gap { .. } => None,
        })
        .unwrap_or(m.next_part_id());
    (0..first)
        .filter(|&id| matches!(m.part(id), crate::PartLookup::Found(_)))
        .count()
}

/// The two streams of the review on one budget, sharing a retention pool
/// too small for both: what is kept goes early and is counted, oldest
/// first, and no live push is refused.
#[test]
fn a_full_retention_pool_releases_kept_media_and_never_live_media() {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(10_000_000));
    let pool = Arc::new(FragmentQuota::new(2_000_000));
    let mut b = Steady::new(cfg, &quota, 20_000);
    let mut a = Steady::new(cfg, &quota, 20_000);
    b.m.set_retention_pool(pool.clone());
    a.m.set_retention_pool(pool.clone());
    for i in 0..90 * 30 {
        b.push().expect("B is never refused");
        if i >= 30 * 30 {
            a.push().expect("A is never refused");
        }
        assert!(pool.used() <= 2_000_000);
    }
    let early = b.m.parts_released_early() + a.m.parts_released_early();
    assert!(early > 0, "the pool filled");
    assert!(kept(&b.m) > 0, "B still kept what fitted");
    drop((a, b));
    assert_eq!((quota.used(), pool.used()), (0, 0), "nothing leaks");
}

/// With no room in the pool at all, a stream with nothing kept releases
/// each segment as it leaves the window, as main did: its live budget use
/// is main's, and only the new record goes.
#[test]
fn an_empty_retention_pool_releases_each_segment_as_it_leaves() {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(512 << 20));
    let mut s = Steady::new(cfg, &quota, 20_000);
    s.m.set_retention_pool(Arc::new(FragmentQuota::new(0)));
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut evicted = 0;
    for _ in 0..60 * 30 {
        s.push().expect("never refused");
        h.update((quota.used() as u64).to_le_bytes());
        assert_eq!(kept(&s.m), 0, "nothing kept");
        evicted = s.m.media_sequence();
    }
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        digest, "cdc0e50dc2be513fd7fb6e6cac05033d346074b4b58bce7b2f16fd56aad34f3c",
        "main's live budget use"
    );
    assert!(evicted > 0 && s.m.parts_released_early() > 0);
    // Codec epochs too: an old epoch's init that only kept media would
    // hold cannot move to the empty pool, and goes as on main.
    assert_eq!(
        epochs_live_use(Some(Arc::new(FragmentQuota::new(0)))).0,
        "3d95a9f7a1f4acaa432668ac10b2e175d02ca2df57b6d2a1fec5b87f420f09cf"
    );
}

/// A new codec epoch under a tight shared budget: B runs alone for 30 s,
/// then A joins on the same 9.5 MB budget and changes epoch at its frame
/// 90, which needs room for its new init and for sealing its open part.
/// What B keeps is in the pool, not the budget, so A's epoch change is
/// admitted, as on main. (Two such streams need about 9.64 MB at their
/// peak on main, so the test stops there.)
#[test]
fn a_new_epoch_is_admitted_beside_another_stream_s_kept_media() {
    let cfg = MuxerConfig {
        segment_count: 7,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(9_500_000));
    let mut b = Steady::new(cfg, &quota, 20_000);
    for _ in 0..30 * 30 {
        b.push().expect("B is never refused");
    }
    let mut a = Steady::new(cfg, &quota, 20_000);
    for _ in 0..90 {
        b.push().expect("B is never refused");
        a.push().expect("A is never refused");
    }
    assert!(kept(&b.m) > 0, "B keeps media past its window");
    a.m.reconfigure_tracks(Some(video()), Some(aac()), NOW)
        .expect("A's new epoch is admitted");
    assert_eq!(a.m.current_initialization_id(), 1);
}

/// The live budget's use after every push, hashed, and its peak: what main
/// charged, the window and the open segment only.
fn live_use(mut push: impl FnMut() -> bool, quota: &Arc<FragmentQuota>) -> (String, usize) {
    use sha2::{Digest, Sha256};
    let (mut h, mut peak) = (Sha256::new(), 0);
    while push() {
        h.update((quota.used() as u64).to_le_bytes());
        peak = peak.max(quota.used());
    }
    (
        h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        peak,
    )
}

fn steady_live_use(segment_count: usize) -> (String, usize) {
    let cfg = MuxerConfig {
        segment_count,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(512 << 20));
    let mut m =
        Muxer::with_fragment_quota(cfg, Some(video()), Some(aac()), quota.clone()).expect("muxer");
    let (mut frame, mut next_audio) = (0i64, 0i64);
    live_use(
        || {
            if frame == 60 * 30 {
                return false;
            }
            let dts = frame * 3000;
            while next_audio <= dts {
                m.push_audio(next_audio, Bytes::from_static(b"aac"))
                    .expect("AAC");
                next_audio += 1920;
            }
            m.push_video(
                dts,
                dts,
                frame % 30 == 0,
                Bytes::from(vec![0u8; 20_000]),
                NOW,
            )
            .expect("video");
            frame += 1;
            true
        },
        &quota,
    )
}

fn epochs_live_use(pool: Option<Arc<FragmentQuota>>) -> (String, usize) {
    let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
    let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
    let cfg = MuxerConfig {
        segment_count: 8,
        ..MuxerConfig::default()
    };
    let quota = Arc::new(FragmentQuota::new(256 << 20));
    let mut ingest = Some(crate::TsIngest::with_fragment_quota(cfg, quota.clone()));
    let chunks: Vec<&[u8]> = (0..16)
        .flat_map(|epoch| {
            let bytes = if epoch % 2 == 0 {
                portrait.as_slice()
            } else {
                landscape.as_slice()
            };
            bytes.chunks(1316).chain(std::iter::once(&[][..]))
        })
        .collect();
    let mut next = 0;
    live_use(
        || {
            let Some(chunk) = chunks.get(next) else {
                return false;
            };
            next += 1;
            let current = ingest.as_mut().expect("ingest");
            if chunk.is_empty() {
                // The end of an epoch: the publisher reconnects.
                let mut held = current.take_muxer().expect("fixture");
                held.interrupt(NOW);
                ingest = Some(crate::TsIngest::resume(cfg, held));
            } else {
                current.push(chunk, NOW).expect("fixture");
            }
            if let (Some(pool), Some(m)) = (&pool, ingest.as_mut().and_then(|i| i.muxer_mut())) {
                m.set_retention_pool(pool.clone());
            }
            true
        },
        &quota,
    )
}

#[test]
fn the_live_budget_holds_what_main_held() {
    let actual = [
        ("steady, segment_count 7", steady_live_use(7)),
        ("steady, segment_count 24", steady_live_use(24)),
        ("epochs, segment_count 8", epochs_live_use(None)),
    ];
    for (name, (digest, peak)) in &actual {
        println!("{name}: {digest} peak {peak}");
    }
    // Taken from main (9eb8745), which freed media as it left the window.
    let expected = [
        (
            "steady, segment_count 7",
            "cdc0e50dc2be513fd7fb6e6cac05033d346074b4b58bce7b2f16fd56aad34f3c",
        ),
        (
            "steady, segment_count 24",
            "882c3b7017d268115cbbdfd8264a6901b723bcd10cd4e7d986336b3773fe9e97",
        ),
        (
            "epochs, segment_count 8",
            "3d95a9f7a1f4acaa432668ac10b2e175d02ca2df57b6d2a1fec5b87f420f09cf",
        ),
    ];
    let actual: Vec<(&str, &str)> = actual.iter().map(|(n, (d, _))| (*n, d.as_str())).collect();
    assert_eq!(actual, expected);
}
