//! Bounded opaque-frame scheduling metadata and host ownership bookkeeping.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_core::realtime::{Changes, Frame, Kind, Limits, Queue};
use std::collections::BTreeMap;

fn release(owned: &mut BTreeMap<u32, u32>, changes: Changes) {
    for id in changes.discarded {
        assert!(
            owned.remove(&id).is_some(),
            "release an owned frame exactly once"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 8192 {
        return;
    }
    if let Some(raw) = data.get(..16) {
        let word = |at| u32::from_le_bytes(raw[at..at + 4].try_into().expect("word"));
        let _ = Limits::new(word(0), word(4), word(8), u64::from(word(12)));
    }
    let mut queue = Queue::new(Limits::new(4096, 16, 8, 20_000).expect("bounded fixture"));
    let mut owned = BTreeMap::new();
    let mut now = 0u64;
    for (index, raw) in data.chunks_exact(20).take(256).enumerate() {
        now = now.saturating_add(u64::from(raw[13]) * 100);
        let operation_now = if raw[0] & 128 != 0 {
            now.saturating_sub(1)
        } else {
            now
        };
        match raw[0] % 5 {
            0 | 1 => {
                let raw_id = u32::from_le_bytes(raw[4..8].try_into().expect("ID"));
                let frame = Frame {
                    id: if raw[1] & 128 != 0 {
                        raw_id
                    } else {
                        index as u32 + 1
                    },
                    stream: u32::from(raw[2] % 9),
                    kind: match raw[1] % 3 {
                        0 => Kind::Audio,
                        1 => Kind::VideoKey,
                        _ => Kind::VideoDelta,
                    },
                    sequence: u32::from_le_bytes(raw[8..12].try_into().expect("sequence")),
                    bytes: u32::from(u16::from_le_bytes([raw[14], raw[15]])),
                    deadline_us: now.saturating_add(u64::from(raw[16]) * 100),
                };
                if let Ok(result) = queue.offer(frame, operation_now) {
                    release(&mut owned, result.changes);
                    if result.accepted {
                        assert!(owned.insert(frame.id, frame.bytes).is_none());
                    }
                }
            }
            2 => {
                if let Ok(result) = queue.pop(operation_now) {
                    release(&mut owned, result.changes);
                    if let Some(frame) = result.frame {
                        assert_eq!(owned.remove(&frame.id), Some(frame.bytes));
                    }
                }
            }
            3 => {
                if let Ok(changes) = queue.tick(operation_now) {
                    release(&mut owned, changes);
                }
            }
            _ => release(&mut owned, queue.close()),
        }
        assert_eq!(queue.frames(), owned.len());
        assert_eq!(queue.bytes(), owned.values().sum::<u32>());
        assert!(queue.bytes() <= 4096);
        assert!(queue.frames() <= 16);
        assert!(queue.streams() <= 8);
    }
    release(&mut owned, queue.close());
    assert!(owned.is_empty());
});
