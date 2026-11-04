# Future latency and frame-rate roadmap

These are candidates for releases after v0.9.1, not promises of measured speedups. v0.9.1 changes
USB controller support only. See the [source-backed review](reviews/2026-11-03-performance-review.md)
for exact code locations and proposed verification.

## First: establish comparable measurements

- Benchmark the same game, scene, PCs, USB controller, network, resolution, refresh rate, bitrate,
  codec and visual quality in Connect and Parsec. Measure actual input-to-photon, not only draw
  timestamps. Report p50/p95/p99, stalls and frame delivery alongside CPU/GPU load.
- Use existing CSV traces to separate capture, conversion, encode, send queue, network/FEC,
  reorder, decode queue, decode and draw. Add presentation correlation and capture-to-input-enqueue
  timing where current metrics stop short.
- Measure sustained 60/120/144/240 Hz workloads on NVIDIA, AMD and Intel with Windows and Linux
  viewers. A driver accepting 240 Hz is not proof of sustained 240 fps.
- Verify the path actually selected: hardware encode/decode, shared GPU textures and NASM-enabled
  OpenH264 fallback. Correct stale performance documentation that describes mandatory readback.

## Priority 1: eliminate avoidable latency tails

1. **Bound input queue age.** Instrument the unbounded viewer/injector queues. Coalesce stale
   motion/analog updates between edges while preserving press, release, removal and neutralization.
   Avoid a blanket switch to unreliable input packets.
2. **Give reorder recovery a real timer.** A declared 12 ms wait currently expires only when
   another video datagram arrives. Test recovery after a missing reference followed by silence or
   audio/cursor-only traffic. Later tune the wait from measured reordering rather than removing it.
3. **Recover from decoder overload by frame age.** Replace reliance on a 24-frame queue with a
   bounded time budget. Preserve reference chains, or reset through a fresh keyframe when recovery
   is cheaper than displaying old frames. Test both transient and sustained overload.
4. **React earlier to congestion.** Evaluate queue-age/throughput feedback against the current
   250 ms control tick, 25/60 ms backlog thresholds and capture gating. Include FEC/audio overhead;
   measure bitrate oscillation, recovery time and quality under stepped bandwidth reductions.

## Priority 2: improve sustained frame rate under load

- **Profile the bottleneck before increasing concurrency.** At 240 fps each pipeline stage must
  sustain one frame every 4.17 ms. Inspect per-stage p95/p99 and GPU contention while a real game
  is rendering, not just a test pattern. Avoid accumulating more in-flight work to hide overload.
- **Reduce per-frame allocation/copy work where traces justify it.** Profile packetization/FEC
  buffers and source snapshots, reuse bounded buffers where useful, and verify CPU fallback
  assembly. Preserve the already-existing shared-texture and GPU capture paths.
- **Study GPU synchronization.** The shared texture decoder still waits for GPU completion.
  Investigate GPU-to-GPU synchronization with reusable bounded textures, correct queue ownership,
  fences and device-loss recovery only if those waits limit throughput or tail latency.
- **Compare encoder notification waits.** Measure event-driven Media Foundation waits against
  the existing yield/sleep loop, including CPU use, scheduling delay and busy-game behavior.
- **Keep audio off the video receiver if it blocks.** Measure inline Opus decode and playback
  lock time; consider a dedicated bounded audio worker and adaptive buffering when supported by
  underrun/jitter evidence. Protect A/V sync and loss concealment.
- **Evaluate supported display presentation modes.** Verify compositor, FIFO/mailbox/tearing and
  VRR behavior on each platform. The existing low-latency surface configuration is already enabled.
  Any tearing/latency tradeoff should be explicit and supported by presentation measurements.
- **Consider adaptive resolution/frame cadence for sustained overload.** When encode, decode or
  bandwidth cannot sustain the requested mode, reducing resolution/cadence can keep input and
  video near live time. Preserve resize negotiation and avoid rapid quality changes.

## Larger independent projects

- Hardware AV1 may improve quality per bit, but needs real encode/decode support and negotiation
  evidence; codec efficiency alone does not guarantee lower latency or higher fps.
- Linux hosting needs its own capture, hardware encode and authorized input-injection design for
  Wayland/X11. It is separate from building a Linux viewer or fixing USB button layouts.
- Additional platform decode backends or vendor-specific encoder APIs are justified only by
  measured compatibility/performance gaps in the existing paths.

Each selected follow-up should ship with one reproducible before/after workload, reference/input
correctness tests, p95/p99 latency and frame-delivery evidence, and a documented fallback. Do not
bundle this roadmap into a controller bugfix release.
