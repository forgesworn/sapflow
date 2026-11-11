//! Stream-local subscriptions and coalesced worker wakeups.
//!
//! A worker holds the stream alive while subscribed. Subscription removal and
//! publishing take the same lock, then the worker's pending lock, so removing
//! a subscription also removes its final queued notification. No queued key
//! can outlive its stream and later address a replacement at the same address.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use mio::Waker;

pub(super) struct WorkerWake {
    waker: Waker,
    pending: Mutex<HashSet<usize>>,
}

impl WorkerWake {
    pub(super) fn new(waker: Waker) -> Self {
        Self {
            waker,
            pending: Mutex::new(HashSet::new()),
        }
    }

    fn key(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }

    pub(super) fn changed(&self, stream: usize) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let was_empty = pending.is_empty();
        pending.insert(stream);
        // Coalesce both repeated changes to a stream and different streams in
        // the same pending batch. The worker drains after observing the wake.
        if was_empty {
            let _ = self.waker.wake();
        }
    }

    pub(super) fn take_into(&self, into: &mut HashSet<usize>) {
        debug_assert!(into.is_empty());
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::swap(&mut *pending, into);
    }

    fn forget(&self, stream: usize) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&stream);
    }
}

/// Allocates entries only for workers with requests parked on this stream.
/// Weak references avoid keeping failed workers or their pollers alive.
#[derive(Default)]
pub(crate) struct StreamNotifications {
    workers: Mutex<HashMap<usize, Weak<WorkerWake>>>,
}

impl StreamNotifications {
    pub(super) fn subscribe(&self, worker: &Arc<WorkerWake>) {
        self.workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(worker.key(), Arc::downgrade(worker));
    }

    pub(super) fn unsubscribe(&self, worker: &WorkerWake, stream: usize) {
        let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
        workers.remove(&worker.key());
        // Keep the subscription lock until the queued key is gone: a notifier
        // that was already running cannot put it back after this point.
        worker.forget(stream);
    }

    pub(crate) fn notify(&self, stream: usize) {
        self.workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, worker| {
                if let Some(worker) = worker.upgrade() {
                    worker.changed(stream);
                    true
                } else {
                    false
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mio::{Events, Poll, Token};
    use std::time::Duration;

    fn worker() -> (Poll, Arc<WorkerWake>) {
        let poll = Poll::new().expect("poll");
        let worker = Arc::new(WorkerWake::new(
            Waker::new(poll.registry(), Token(1)).expect("waker"),
        ));
        (poll, worker)
    }

    #[test]
    fn only_subscribed_workers_wake_and_repeated_changes_coalesce() {
        let stream = StreamNotifications::default();
        let (mut wanted_poll, wanted) = worker();
        let (mut other_poll, other) = worker();
        stream.subscribe(&wanted);
        for _ in 0..1000 {
            stream.notify(7);
        }
        let mut events = Events::with_capacity(8);
        wanted_poll
            .poll(&mut events, Some(Duration::ZERO))
            .expect("wanted poll");
        assert_eq!(events.iter().count(), 1);
        other_poll
            .poll(&mut events, Some(Duration::ZERO))
            .expect("other poll");
        assert_eq!(events.iter().count(), 0);
        let mut pending = HashSet::new();
        wanted.take_into(&mut pending);
        assert_eq!(pending, HashSet::from([7]));
        pending.clear();
        other.take_into(&mut pending);
        assert!(pending.is_empty());
    }

    #[test]
    fn concurrent_publish_and_unsubscribe_cannot_requeue_a_retired_identity() {
        let stream = StreamNotifications::default();
        let (_poll, worker) = worker();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..10_000 {
                    stream.notify(17);
                }
            });
            for _ in 0..1000 {
                stream.subscribe(&worker);
                stream.unsubscribe(&worker, 17);
                let mut pending = HashSet::new();
                worker.take_into(&mut pending);
                assert!(
                    pending.is_empty(),
                    "unsubscribe fences concurrent notifications"
                );
            }
        });
        assert!(stream.workers.lock().expect("subscriptions").is_empty());
    }

    #[test]
    fn publishing_while_a_batch_is_drained_rearms_the_worker() {
        let stream = StreamNotifications::default();
        let (mut poll, worker) = worker();
        stream.subscribe(&worker);
        stream.notify(1);
        let mut events = Events::with_capacity(8);
        poll.poll(&mut events, Some(Duration::ZERO))
            .expect("first wake");
        assert_eq!(events.iter().count(), 1);
        let barrier = std::sync::Barrier::new(2);
        let mut drained = HashSet::new();
        std::thread::scope(|scope| {
            // Emulate take_into's atomic drain while the producer attempts to
            // publish. It must wait for this lock then rearm an empty batch.
            let mut pending = worker.pending.lock().expect("pending lock");
            scope.spawn(|| {
                barrier.wait();
                stream.notify(1);
            });
            barrier.wait();
            std::mem::swap(&mut *pending, &mut drained);
            drop(pending);
        });
        assert_eq!(drained, HashSet::from([1]));
        poll.poll(&mut events, Some(Duration::from_secs(1)))
            .expect("second wake");
        assert_eq!(events.iter().count(), 1);
        drained.clear();
        worker.take_into(&mut drained);
        assert_eq!(drained, HashSet::from([1]));
    }

    #[test]
    fn dropped_worker_does_not_remain_owned_by_a_stream() {
        let stream = StreamNotifications::default();
        let (_poll, worker) = worker();
        stream.subscribe(&worker);
        let weak = Arc::downgrade(&worker);
        drop(worker);
        assert!(weak.upgrade().is_none());
        stream.notify(1);
        assert!(stream.workers.lock().expect("subscriptions").is_empty());
    }
}
