//! The host's half of the performance trace: a record per frame sent, per colour conversion
//! timed on the graphics card and per remote input event, kept in bounded rings. Summarised for
//! the viewer about once a second, and written as CSV when the session ends if a trace directory
//! is set. Every time here is on the host's clock ([`connect_proto::now_micros`]). See METRICS.md.

use std::{fmt::Write as _, path::Path};

use connect_perf::{CsvRecord, ProcessMonitor, TraceRing, cell, percentiles};
use connect_proto::{Codec, perf::HostPerf};

/// One frame, from capture to handing its datagrams to the network stack.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameRecord {
    pub seq: u32,
    pub keyframe: bool,
    pub codec: Codec,
    /// A static picture encoded again, as a keyframe the viewer asked for: it wasn't held, just
    /// kept.
    pub resent: bool,
    /// Which capture the picture came from; matches [`ConvertRecord::capture`].
    pub capture: u64,
    pub bytes: u32,
    /// When the picture was staged for the encoder.
    pub capture_time_us: u64,
    /// From Windows putting the image on screen to capture picking it up; 0 where unknown.
    pub present_to_capture_us: u32,
    /// From acquiring the image to it being staged (copy, and submitting the conversion).
    pub capture_us: u32,
    /// Staged until the encoder started on it.
    pub held_us: u32,
    pub encode_us: u32,
    /// From the encoder finishing to the session starting to send it.
    pub queue_us: u32,
    pub packetize_us: u32,
    /// The call handing the datagrams to the network stack.
    pub send_us: u32,
    /// From `capture_time_us` until packetizing began: the figure the frame's header carries.
    pub sent_after_us: u32,
    pub datagrams: u16,
    pub recovery: u16,
    /// Bytes already waiting in the connection's datagram buffer when this frame was sent.
    pub queued_bytes: u32,
    pub bitrate_kbps: u32,
    /// The path's datagrams got smaller, and the frame was dropped.
    pub dropped: bool,
}

impl CsvRecord for FrameRecord {
    const HEADER: &'static str = "seq,keyframe,codec,resent,capture_index,bytes,capture_time_us,present_to_capture_us,\
capture_us,held_us,encode_us,queue_us,packetize_us,send_us,sent_after_us,datagrams,recovery_datagrams,\
queued_bytes,bitrate_kbps,dropped";

    fn write_row(&self, out: &mut String) {
        let _ = write!(
            out,
            "{},{},{},{},{},{},{},",
            self.seq,
            self.keyframe as u8,
            self.codec,
            self.resent as u8,
            self.capture,
            self.bytes,
            self.capture_time_us
        );
        cell(out, (self.present_to_capture_us > 0).then_some(self.present_to_capture_us));
        let _ = write!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            self.capture_us,
            self.held_us,
            self.encode_us,
            self.queue_us,
            self.packetize_us,
            self.send_us,
            self.sent_after_us,
            self.datagrams,
            self.recovery,
            self.queued_bytes,
            self.bitrate_kbps,
            self.dropped as u8
        );
    }
}

/// One colour conversion's run time on the graphics card.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ConvertRecord {
    pub capture: u64,
    pub gpu_us: u32,
}

impl CsvRecord for ConvertRecord {
    const HEADER: &'static str = "capture_index,convert_gpu_us";

    fn write_row(&self, out: &mut String) {
        let _ = write!(out, "{},{}", self.capture, self.gpu_us);
    }
}

/// What became of a remote input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InputOutcome {
    /// Handed to Windows (`SendInput`) or the gamepad driver.
    #[default]
    Injected,
    /// Windows refused it, e.g. an elevated window has the focus.
    #[cfg_attr(not(windows), allow(dead_code))]
    Refused,
    /// Dropped because the host is using that device themselves.
    #[cfg_attr(not(windows), allow(dead_code))]
    HostPriority,
    /// Dropped because the session doesn't allow that device.
    NotAllowed,
    /// Handed to a test channel instead of the system.
    Channel,
}

impl InputOutcome {
    fn name(self) -> &'static str {
        match self {
            InputOutcome::Injected => "injected",
            InputOutcome::Refused => "refused",
            InputOutcome::HostPriority => "host_priority",
            InputOutcome::NotAllowed => "not_allowed",
            InputOutcome::Channel => "channel",
        }
    }
}

/// One remote input event.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InputRecord {
    pub seq: u32,
    pub kind: &'static str,
    /// When the viewer sent it, on the viewer's clock: not comparable with the others here
    /// without the clock offset.
    pub client_time_us: u64,
    /// When it came off the input stream.
    pub received_us: u64,
    /// When it was acted on (or dropped).
    pub done_us: u64,
    pub outcome: InputOutcome,
}

impl CsvRecord for InputRecord {
    const HEADER: &'static str = "seq,kind,client_time_us_viewer_clock,received_us,done_us,inject_us,outcome";

    fn write_row(&self, out: &mut String) {
        let _ = write!(
            out,
            "{},{},{},{},{},{},{}",
            self.seq,
            self.kind,
            self.client_time_us,
            self.received_us,
            self.done_us,
            self.done_us.saturating_sub(self.received_us),
            self.outcome.name()
        );
    }
}

/// When a remote input event was sent and received, carried with it to where it's acted on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputStamp {
    pub seq: u32,
    pub client_time_us: u64,
    pub received_us: u64,
}

impl InputStamp {
    pub fn record(self, kind: &'static str, outcome: InputOutcome) -> InputRecord {
        InputRecord {
            seq: self.seq,
            kind,
            client_time_us: self.client_time_us,
            received_us: self.received_us,
            done_us: connect_proto::now_micros(),
            outcome,
        }
    }
}

/// A session's trace.
pub(crate) struct HostTrace {
    pub frames: TraceRing<FrameRecord>,
    pub conversions: TraceRing<ConvertRecord>,
    pub inputs: TraceRing<InputRecord>,
    /// When the session started, in Unix seconds, to name its files.
    session: u64,
}

impl HostTrace {
    pub fn new() -> Self {
        let capacity = connect_perf::ring_capacity();
        Self {
            frames: TraceRing::new(capacity),
            conversions: TraceRing::new(capacity),
            inputs: TraceRing::new(capacity),
            session: connect_perf::session_stamp(),
        }
    }

    fn dropped(&self) -> u64 {
        self.frames.dropped_busy() + self.conversions.dropped_busy() + self.inputs.dropped_busy()
    }

    /// Writes the trace to `dir` as three CSV files. Blocks on file I/O.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let path = |what| connect_perf::trace_path(dir, "host", self.session, what);
        connect_perf::write_csv(&path("frames"), &self.frames.snapshot())?;
        connect_perf::write_csv(&path("convert"), &self.conversions.snapshot())?;
        connect_perf::write_csv(&path("input"), &self.inputs.snapshot())
    }
}

/// Turns the trace into [`HostPerf`] reports, each covering the records since the last.
pub(crate) struct Reporter {
    marks: [u64; 3],
    process: ProcessMonitor,
}

impl Reporter {
    pub fn new() -> Self {
        Self { marks: [0; 3], process: ProcessMonitor::new() }
    }

    /// The figures since the last report; `None` while tracing is off.
    pub fn report(&mut self, trace: &HostTrace) -> Option<HostPerf> {
        if !connect_perf::enabled() {
            return None;
        }
        let (frames, mark) = trace.frames.since(self.marks[0]);
        self.marks[0] = mark;
        let (conversions, mark) = trace.conversions.since(self.marks[1]);
        self.marks[1] = mark;
        let (inputs, mark) = trace.inputs.since(self.marks[2]);
        self.marks[2] = mark;

        let of = |pick: &dyn Fn(&FrameRecord) -> Option<u32>| {
            percentiles(&mut frames.iter().filter_map(pick).map(i64::from).collect::<Vec<_>>())
        };
        let sent = |f: &FrameRecord| !f.dropped;
        let injected: Vec<&InputRecord> = inputs.iter().filter(|i| i.outcome == InputOutcome::Injected).collect();
        Some(HostPerf {
            process: self.process.sample(),
            capture: of(&|f| Some(f.capture_us)),
            convert_gpu: percentiles(&mut conversions.iter().map(|c| i64::from(c.gpu_us)).collect::<Vec<_>>()),
            held: of(&|f| (!f.resent).then_some(f.held_us)),
            encode: of(&|f| Some(f.encode_us)),
            queue: of(&|f| Some(f.queue_us)),
            packetize: of(&|f| sent(f).then_some(f.packetize_us)),
            send: of(&|f| sent(f).then_some(f.send_us)),
            input_inject: percentiles(
                &mut injected.iter().map(|i| i.done_us.saturating_sub(i.received_us) as i64).collect::<Vec<_>>(),
            ),
            input_transit_raw: percentiles(
                &mut inputs.iter().map(|i| i.received_us as i64 - i.client_time_us as i64).collect::<Vec<_>>(),
            ),
            trace_dropped: trace.dropped(),
        })
    }
}

#[cfg(test)]
mod tests {
    use connect_perf::Percentiles;

    use super::*;

    fn p50(p: Option<Percentiles>) -> Option<i64> {
        p.map(|p| p.p50)
    }

    #[test]
    fn reports_cover_only_records_since_the_last() {
        let trace = HostTrace::new();
        let mut reporter = Reporter::new();
        for seq in 0..10 {
            trace.frames.push(FrameRecord { seq, capture_us: 1_000 + seq, encode_us: 3_000, ..Default::default() });
        }
        trace.conversions.push(ConvertRecord { capture: 1, gpu_us: 250 });
        let stamp = InputStamp { seq: 0, client_time_us: 5_000, received_us: 9_000 };
        trace.inputs.push(InputRecord { done_us: 9_300, ..stamp.record("key", InputOutcome::Injected) });
        trace.inputs.push(stamp.record("key", InputOutcome::HostPriority));

        let first = reporter.report(&trace).unwrap();
        assert_eq!(first.capture.unwrap().count, 10);
        assert_eq!(p50(first.capture), Some(1_004));
        assert_eq!(p50(first.encode), Some(3_000));
        assert_eq!(p50(first.convert_gpu), Some(250));
        // Only the injected event counts towards injection time; both towards transit.
        assert_eq!((first.input_inject.unwrap().count, p50(first.input_inject)), (1, Some(300)));
        assert_eq!((first.input_transit_raw.unwrap().count, p50(first.input_transit_raw)), (2, Some(4_000)));
        assert!(first.process.is_some());

        let second = reporter.report(&trace).unwrap();
        assert_eq!((second.capture, second.convert_gpu, second.input_inject), (None, None, None));
        trace.frames.push(FrameRecord { seq: 10, capture_us: 7, resent: true, ..Default::default() });
        let third = reporter.report(&trace).unwrap();
        assert_eq!(p50(third.capture), Some(7));
        assert_eq!(third.held, None, "a re-sent picture wasn't held");
    }

    #[test]
    fn records_stay_small() {
        assert!(size_of::<FrameRecord>() <= 96, "{} bytes", size_of::<FrameRecord>());
        assert!(size_of::<InputRecord>() <= 64, "{} bytes", size_of::<InputRecord>());
    }

    #[test]
    fn csv_rows_match_the_header() {
        let frame = FrameRecord { seq: 3, keyframe: true, codec: Codec::H265, bytes: 9_000, ..Default::default() };
        let mut row = String::new();
        frame.write_row(&mut row);
        assert_eq!(row.split(',').count(), FrameRecord::HEADER.split(',').count(), "{row}");
        assert!(row.starts_with("3,1,H.265,0,"), "{row}");
        let input = InputStamp { seq: 1, client_time_us: 10, received_us: 20 }.record("key", InputOutcome::Refused);
        row.clear();
        input.write_row(&mut row);
        assert_eq!(row.split(',').count(), InputRecord::HEADER.split(',').count(), "{row}");
        assert!(row.ends_with(",refused"));
    }
}
