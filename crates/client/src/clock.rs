//! The offset between this PC's clock and the host's, estimated from pings.
//!
//! The host stamps its reply somewhere between the ping leaving and the reply arriving, so
//! taking the middle of the round trip is off by at most half of it. The fastest recent round
//! trip gives the tightest bound. Only recent pings count, because the two PCs' clocks drift
//! apart: PC clocks commonly differ in rate by tens of µs per second.

use std::{collections::VecDeque, time::Duration, time::Instant};

/// Pings older than this no longer count.
const WINDOW: Duration = Duration::from_secs(10);

/// This PC's clock minus the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockOffset {
    pub offset_us: i64,
    /// How far the true offset can be from `offset_us`: half the round trip it came from. Every
    /// figure that spans both PCs carries this uncertainty, and any asymmetry between the two
    /// directions of the path lands within it.
    pub uncertainty_us: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ClockSync {
    /// Recent pings: when the reply came, its round trip, and the offset it implies.
    samples: VecDeque<(Instant, u64, i64)>,
}

impl ClockSync {
    /// Adds a ping sent at `sent_us` and answered at `received_us` (this clock), which the host
    /// stamped `host_us` (its clock). Returns the best estimate now.
    pub fn add(&mut self, sent_us: u64, host_us: u64, received_us: u64, at: Instant) -> Option<ClockOffset> {
        if received_us >= sent_us {
            let rtt = received_us - sent_us;
            let offset = (sent_us + rtt / 2) as i64 - host_us as i64;
            self.samples.push_back((at, rtt, offset));
        }
        while self.samples.len() > 1 && self.samples.front().is_some_and(|(t, ..)| at.duration_since(*t) > WINDOW) {
            self.samples.pop_front();
        }
        self.best()
    }

    /// The offset from the fastest recent round trip (the newest, among equals).
    pub fn best(&self) -> Option<ClockOffset> {
        let (_, rtt, offset) = self.samples.iter().rev().min_by_key(|(_, rtt, _)| *rtt)?;
        Some(ClockOffset { offset_us: *offset, uncertainty_us: rtt.div_ceil(2) })
    }

    /// The fastest recent round trip, µs.
    pub fn min_rtt_us(&self) -> Option<u64> {
        self.samples.iter().map(|(_, rtt, _)| *rtt).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_sign_and_uncertainty() {
        let mut sync = ClockSync::default();
        let now = Instant::now();
        // This clock is 5 s ahead of the host's; 10 ms each way.
        let best = sync.add(105_000_000, 100_010_000, 105_020_000, now).unwrap();
        assert_eq!(best, ClockOffset { offset_us: 5_000_000, uncertainty_us: 10_000 });
        // A host time plus the offset is this clock's time.
        assert_eq!(100_010_000 + best.offset_us, 105_010_000);
        // Behind instead: a negative offset.
        let mut sync = ClockSync::default();
        let best = sync.add(1_000, 3_000_500, 2_000, now).unwrap();
        assert_eq!(best.offset_us, 1_500 - 3_000_500);
    }

    #[test]
    fn the_fastest_round_trip_wins() {
        let mut sync = ClockSync::default();
        let now = Instant::now();
        sync.add(0, 10_000, 40_000, now); // 40 ms round trip, offset +10 ms
        let best = sync.add(100_000, 95_000, 110_000, now).unwrap(); // 10 ms, offset +10 ms
        assert_eq!(best, ClockOffset { offset_us: 10_000, uncertainty_us: 5_000 });
        // A slower one later doesn't displace it.
        let best = sync.add(200_000, 180_000, 260_000, now).unwrap();
        assert_eq!(best.uncertainty_us, 5_000);
        assert_eq!(sync.min_rtt_us(), Some(10_000));
    }

    #[test]
    fn old_pings_expire_so_drift_is_followed() {
        let mut sync = ClockSync::default();
        let start = Instant::now();
        sync.add(0, 0, 2_000, start); // fast, offset +1 ms
        // Twelve seconds later the clocks have drifted 1 ms; only a slower ping is recent.
        let best = sync.add(12_000_000, 11_998_000, 12_008_000, start + Duration::from_secs(12)).unwrap();
        assert_eq!(best, ClockOffset { offset_us: 6_000, uncertainty_us: 4_000 });
        // A reply from before the ping went (a broken clock) is ignored.
        assert_eq!(sync.add(5, 0, 4, start + Duration::from_secs(13)), Some(best));
    }
}
