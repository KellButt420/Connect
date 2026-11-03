//! The host's performance figures, sent to the viewer about once a second with
//! [`HostMsg::VideoStats`](crate::control::HostMsg::VideoStats) for its stats overlay. Every
//! duration is measured on the host's own clock; see METRICS.md.

pub use connect_perf::{Percentiles, ProcessLoad};
use serde::{Deserialize, Serialize};

/// Percentiles (µs) over the frames and input the host handled since its last report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct HostPerf {
    /// The host process's CPU and memory use.
    pub process: Option<ProcessLoad>,
    /// Acquiring the image from the screen until it's staged for the encoder (copy, and
    /// submitting the colour conversion).
    pub capture: Option<Percentiles>,
    /// The colour conversion's run time on the graphics card, from timestamp queries.
    pub convert_gpu: Option<Percentiles>,
    /// A staged picture waiting for its turn to be encoded (network backlog, frame spacing).
    pub held: Option<Percentiles>,
    pub encode: Option<Percentiles>,
    /// The encoded frame waiting for the session to take it.
    pub queue: Option<Percentiles>,
    /// Cutting the frame into datagrams, recovery data included.
    pub packetize: Option<Percentiles>,
    /// Handing the datagrams to the network stack.
    pub send: Option<Percentiles>,
    /// Remote input from arriving to being injected (`SendInput` returning).
    pub input_inject: Option<Percentiles>,
    /// Remote input: the host's arrival time minus the viewer's send time, across the two PCs'
    /// unsynchronised clocks. Meaningless alone; the viewer adds its clock offset to estimate
    /// the one-way time.
    pub input_transit_raw: Option<Percentiles>,
    /// Trace records the host dropped since the session started, because an export or report
    /// was reading the buffer at that moment.
    pub trace_dropped: u64,
}
