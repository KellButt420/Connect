//! A fixed-size trace buffer that the pipeline's threads can always write to without waiting.

use std::sync::{
    Mutex, PoisonError, TryLockError,
    atomic::{AtomicU64, Ordering},
};

/// Keeps the last `capacity` records. Space is reserved up front (unless tracing is off), so
/// writing never allocates.
///
/// A writer that finds the buffer busy (a reader copying it out) drops its record and counts it
/// rather than wait: the threads that write here capture, encode, send, decode and draw video,
/// and must never stall for tracing.
pub struct TraceRing<T> {
    inner: Mutex<Inner<T>>,
    capacity: usize,
    /// Records dropped because the buffer was busy.
    busy: AtomicU64,
}

struct Inner<T> {
    buf: Vec<T>,
    /// Where the next record goes once the buffer is full.
    next: usize,
    /// Records ever written.
    written: u64,
}

impl<T: Copy> TraceRing<T> {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let reserve = if crate::enabled() { capacity } else { 0 };
        Self {
            inner: Mutex::new(Inner { buf: Vec::with_capacity(reserve), next: 0, written: 0 }),
            capacity,
            busy: AtomicU64::new(0),
        }
    }

    /// Adds `record`, replacing the oldest when full. Never blocks; does nothing while tracing
    /// is off.
    #[inline]
    pub fn push(&self, record: T) {
        if !crate::enabled() {
            return;
        }
        let mut inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.busy.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if inner.buf.len() < self.capacity {
            inner.buf.push(record);
        } else {
            let at = inner.next;
            inner.buf[at] = record;
        }
        inner.next = (inner.next + 1) % self.capacity;
        inner.written += 1;
    }

    /// All records kept, oldest first.
    pub fn snapshot(&self) -> Vec<T> {
        self.since(0).0
    }

    /// The records written after the first `mark` (oldest first, at most the capacity), and
    /// the mark to pass next time to get only newer ones.
    pub fn since(&self, mark: u64) -> (Vec<T>, u64) {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let wanted = inner.written.saturating_sub(mark).min(inner.buf.len() as u64) as usize;
        let mut out = Vec::with_capacity(wanted);
        if inner.buf.len() < self.capacity {
            out.extend_from_slice(&inner.buf[inner.buf.len() - wanted..]);
        } else {
            // Full: the oldest record is at `next`.
            let start = (inner.next + self.capacity - wanted) % self.capacity;
            let (tail, head) = (&inner.buf[start..], &inner.buf[..start]);
            if start + wanted <= self.capacity {
                out.extend_from_slice(&tail[..wanted]);
            } else {
                out.extend_from_slice(tail);
                out.extend_from_slice(&head[..wanted - tail.len()]);
            }
        }
        (out, inner.written)
    }

    /// Records ever written, including ones since overwritten.
    pub fn written(&self) -> u64 {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).written
    }

    /// Records dropped because a reader had the buffer at that moment.
    pub fn dropped_busy(&self) -> u64 {
        self.busy.load(Ordering::Relaxed)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_until_full() {
        let ring = TraceRing::new(4);
        for i in 0..3 {
            ring.push(i);
        }
        assert_eq!(ring.snapshot(), [0, 1, 2]);
        assert_eq!(ring.written(), 3);
    }

    #[test]
    fn keeps_the_newest_when_full_oldest_first() {
        let ring = TraceRing::new(4);
        for i in 0..10 {
            ring.push(i);
        }
        assert_eq!(ring.snapshot(), [6, 7, 8, 9], "bounded to capacity, in write order");
        assert_eq!(ring.written(), 10);
        // Wrapping exactly onto the start.
        for i in 10..12 {
            ring.push(i);
        }
        assert_eq!(ring.snapshot(), [8, 9, 10, 11]);
    }

    #[test]
    fn since_returns_only_newer_records() {
        let ring = TraceRing::new(4);
        ring.push(1);
        ring.push(2);
        let (first, mark) = ring.since(0);
        assert_eq!((first, mark), (vec![1, 2], 2));
        assert_eq!(ring.since(mark), (vec![], 2));
        for i in 3..6 {
            ring.push(i);
        }
        assert_eq!(ring.since(mark), (vec![3, 4, 5], 5));
        // More written since the mark than fit: just the newest capacity's worth.
        for i in 6..20 {
            ring.push(i);
        }
        assert_eq!(ring.since(5).0, [16, 17, 18, 19]);
    }

    #[test]
    fn never_reallocates() {
        let ring = TraceRing::new(8);
        let start = ring.inner.lock().unwrap().buf.as_ptr();
        for i in 0..100u64 {
            ring.push(i);
        }
        assert_eq!(ring.inner.lock().unwrap().buf.as_ptr(), start);
        assert_eq!(ring.inner.lock().unwrap().buf.capacity(), 8);
    }

    #[test]
    fn a_busy_ring_drops_instead_of_waiting() {
        let ring = TraceRing::new(4);
        let held = ring.inner.lock().unwrap();
        // Would deadlock if push waited for the lock.
        ring.push(1);
        drop(held);
        assert_eq!(ring.dropped_busy(), 1);
        assert_eq!(ring.written(), 0);
        ring.push(2);
        assert_eq!(ring.snapshot(), [2]);
    }
}
