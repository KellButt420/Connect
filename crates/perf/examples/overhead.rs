//! What tracing costs the threads that write to it: `cargo run --release -p connect-perf --example overhead`.

use std::{hint::black_box, time::Instant};

use connect_perf::{TraceRing, percentiles};

/// About the size of the viewer's frame record.
#[derive(Clone, Copy, Default)]
struct Record {
    _fields: [u64; 20],
}

fn per_op(label: &str, ops: u64, run: impl FnOnce()) {
    let started = Instant::now();
    run();
    println!("{label:<48} {:>8.1} ns", started.elapsed().as_nanos() as f64 / ops as f64);
}

fn main() {
    const N: u64 = 2_000_000;
    let ring = TraceRing::new(4_096);
    per_op("push, tracing on", N, || {
        for i in 0..N {
            ring.push(black_box(Record { _fields: [i; 20] }));
        }
    });
    connect_perf::set_enabled(false);
    per_op("push, tracing off", N, || {
        for i in 0..N {
            ring.push(black_box(Record { _fields: [i; 20] }));
        }
    });
    connect_perf::set_enabled(true);
    per_op("Instant::now()", N, || {
        for _ in 0..N {
            black_box(Instant::now());
        }
    });
    per_op("qpc_now() (0 off Windows)", N, || {
        for _ in 0..N {
            black_box(connect_perf::qpc_now());
        }
    });
    let mut mark = 0;
    per_op("since(): one second of frames at 240 fps", 1_000, || {
        for _ in 0..1_000 {
            for i in 0..240u64 {
                ring.push(Record { _fields: [i; 20] });
            }
            let (records, next) = ring.since(mark);
            mark = next;
            black_box(records);
        }
    });
    per_op("percentiles() of 240 values", 10_000, || {
        for i in 0..10_000i64 {
            let mut values: Vec<i64> = (0..240).map(|v| (v * 7919 + i) % 10_000).collect();
            black_box(percentiles(&mut values));
        }
    });
    per_op("snapshot() of a full ring (export)", 100, || {
        for _ in 0..100 {
            black_box(ring.snapshot());
        }
    });
    println!("record size: {} bytes, ring of 4096: {} KB", size_of::<Record>(), 4_096 * size_of::<Record>() / 1024);
}
