//! Switching tracing off is process-wide, so this runs as its own test binary.

use connect_perf::TraceRing;

#[test]
fn a_disabled_ring_keeps_nothing() {
    let ring = TraceRing::new(4);
    ring.push(1u32);
    connect_perf::set_enabled(false);
    assert!(!connect_perf::enabled());
    ring.push(2);
    ring.push(3);
    assert_eq!(ring.snapshot(), [1], "nothing kept while off");
    assert_eq!((ring.written(), ring.dropped_busy()), (1, 0), "and nothing counted as dropped");
    connect_perf::set_enabled(true);
    ring.push(4);
    assert_eq!(ring.snapshot(), [1, 4]);
}
