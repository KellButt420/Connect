# Connect v0.9.0 latency and performance review

Static review of the supplied v0.9.0 source archive on 2026-11-03. These are follow-up candidates, not v0.9.1 scope. No end-to-end performance benchmark or Parsec comparison was run. Priorities reflect code evidence and likely user impact, not measured savings.

## Assessment

The architecture already contains useful low-latency work: DXGI capture, a shared D3D11 GPU capture/conversion/encode path, low-latency Media Foundation encoding with one frame in flight, negotiated H.264/H.265, QUIC video datagrams with FEC and BBR, adaptive bitrate, reusable CPU buffers, GPU NV12 presentation on a shared decoder/renderer device, independent gamepad forwarding, and performance traces.

Reimplementing those features is not an optimization. The next gains are more likely in overload handling, tail latency, display/presentation behavior and hardware coverage. Outperforming Parsec is an unproven objective, not a conclusion of reading the code. Linux currently cannot host, which is a substantial platform gap beyond this gamepad release.

## Recommended order

### 1. Bound input queue age without losing edges

Evidence: `crates/client/src/lib.rs:493,595,765` uses an unbounded input channel and one reliable ordered stream. `crates/host/src/input/mod.rs:159` adds an unbounded standard channel before Windows injection; there is also a 256-event queue before it. High-rate axes/mouse motion, a stalled path, or a slow injector can accumulate old input. No observed backlog was measured in this review.

Follow-up: instrument age/depth at enqueue, wire write, host receive and injection. Bound queues by a small latency budget; coalesce only redundant analog/motion updates between edges. Preserve press/release, device removal, permissions, and neutralization. Do not change all input to lossy datagrams: a lost release can stick controls. A future split state/edge protocol needs sequencing, deduplication, resynchronization and compatibility design.

Proof: slow-writer/slow-injector tests with sustained motion and rapid taps; bounded memory/age and every required release reaches the host. Separate capture-to-enqueue time from the existing send-to-host estimate.

### 2. Make decoder overload recovery depend on time, not 24 frames

Evidence: `crates/client/src/video.rs:31–76` allows 24 queued compressed frames and publishes a catch-up frame after at most 20 ms without an update. Queue capacity represents roughly 100 ms of stream time at 240 fps or 400 ms at 60 fps if full; these are capacity calculations, not measured added latency. Old queued frames can remain while fresh frames are refused and a new keyframe is requested.

Follow-up: measure oldest queued capture age and adopt a bounded age policy. Preserve reference-chain decoding when catching up cheaply; on sustained overload reset/drop through a decodable keyframe and request recovery, then resume near live time. Do not simply display the newest inter-frame packet or arbitrarily drop reference frames.

Proof: deliberately slow CPU/GPU decoder, transient and sustained overload, resize/codec change, and lossy-path recovery. Track p95/p99 frame age, reference correctness, stalls and bandwidth cost of keyframe requests.

### 3. Drive video reorder expiry with an actual deadline

Evidence: `crates/proto/src/video.rs:37,382,477` declares a 12 ms reorder wait, but checks expiry only in `FrameAssembler::push`. `crates/client/src/video.rs:205` waits for another datagram and skips assembler processing for audio/cursor packets. A gap followed by no new video traffic can therefore wait longer than 12 ms before recovery starts. This is a static control-flow finding, not an observed freeze duration.

Follow-up: expose the assembler's next deadline/expiry action and wake reception using a timer when a reorder gap exists. Consider an adaptive reorder budget later, based on actual reordering/FEC recovery and frame cadence. Do not erase the wait globally: it protects reordered references from false loss.

Proof: one missing reference plus a later completed frame, followed by silence or audio/cursor-only traffic; assert recovery is requested at the deadline without needing another video packet. Retain existing reordering and FEC tests.

### 4. Tighten congestion response against a measured latency budget

Evidence: `crates/host/src/rate.rs:18–33,86–104` evaluates bitrate every 250 ms, cuts at approximately 25/60 ms of queued video, and holds capture only beyond both 50 ms and 128 KiB. The byte floor permits more queued time at lower bitrates. `crates/net/src/lib.rs:23` allows a 2 MiB datagram buffer; that is capacity, not the normal operating queue.

Follow-up: use queue age and observed send throughput to react earlier to overload; include parity and audio traffic in the bandwidth budget. Evaluate time-based capture gating with a safe allowance for an individual frame/keyframe. Avoid shrinking buffers blindly or causing oscillating bitrate/keyframe storms.

Proof: stepped bandwidth drops, burst loss, Wi-Fi-like jitter and relay paths; record queue age, delivery latency, loss, sharpness and recovery time at identical bitrate caps.

### 5. Measure presentation and input-to-photon before changing display modes

Evidence: `app/src/gui/mod.rs:73` already uses eframe's `SurfaceConfig::LOW_LATENCY`; an additional low-latency surface configuration is not missing. `METRICS.md` correctly says draw timestamps precede actual display presentation, and controller send timestamps exclude pre-enqueue capture/mapping time.

Follow-up: verify selected present mode and compositor behavior on X11/Wayland and Windows at each refresh rate. Correlate Windows traces with PresentMon; use suitable Linux compositor/presentation tracing and an external high-speed camera or hardware input/photodiode setup for comparable input-to-photon results. Only then evaluate supported present-mode/VRR choices and a tearing tradeoff.

Proof: identical host/viewer/game/USB controller/network, codec, resolution, bitrate and refresh, compared against Parsec. Report p50/p95/p99, worst stalls, frame delivery, CPU/GPU use and matched visual quality on direct LAN and constrained/relay paths. Do not compare Connect draw estimates with another tool's display latency.

### 6. Profile GPU waits and encoder polling before adding complexity

Evidence: `crates/gpu-video/src/vulkan_decoder.rs:800` waits for GPU completion even on the texture path. Shared textures avoid CPU readback, but CPU/GPU synchronization remains. The actual memory fallback waits at `:872`. `crates/host/src/encode/mf.rs:49–61` yields for the first 3 ms and then sleeps in 200 µs steps while waiting for encoder events.

Follow-up: profile GPU/CPU timelines under a busy game. Consider asynchronously chaining decode completion into render submission, using bounded reusable textures and correct cross-queue ownership, if CPU waits matter. Assess an event-driven encoder wait against the existing yield/sleep loop; do not trade predictable latency for lower idle CPU without measurement.

Proof: NVIDIA/AMD/Intel correctness under load, device-loss/fallback handling, GPU validation, and no texture reuse while still in flight. This is a larger follow-up; no expected millisecond saving is claimed.

### 7. Keep audio work from delaying video reception

Evidence: `crates/client/src/video.rs:212–225` calls `Playback::push_datagram` inline on the datagram receiver. `crates/client/src/audio.rs:349–357` parses/decodes Opus and locks playback state there. Audio's target buffer is already 20 ms (`:13`), so shrinking packet size alone will not remove playback latency.

Follow-up: measure receiver service time and lock contention. If significant, move audio decode to its own bounded worker and evaluate adaptive buffering based on underruns/jitter. Preserve callback nonblocking behavior, mute flushing, loss concealment and A/V alignment.

Proof: simultaneous video/audio under CPU load, no video-receive stalls, bounded audio queue, underrun rates, end-to-end audio delay and drift. Do not lower the buffer just to obtain a smaller nominal number.

## Hardware coverage and documentation

- `PERFORMANCE.md` still contains “remaining limits” about mandatory GPU readback that conflict with its newer shared-texture path and current `GpuDecoder::new` behavior. Benchmark the actual selected path and update those stale statements in a later documentation task.
- Test AMD and Intel hosts/decoders and Linux audio explicitly; repository claims/old measurements are not evidence that every current hardware combination is verified.
- Confirm hardware H.264/H.265, shared-device textures, and NASM-enabled OpenH264 fallback before optimizing a path that users may not be running.
- Linux host capture/encode/input support is a separate substantial feature, not a side effect of shipping a Linux viewer binary.

## Planning validation

Ran `cargo test --locked --offline -p connect-proto --lib input::tests`: **3 passed**, covering framing, permissions and validation. Other recommendations above are static findings/candidates and require their stated proofs. No source changes or performance fixes were made.

Library API cross-check: installed gilrs 0.11.2 source and [gilrs state documentation](https://docs.rs/gilrs/latest/gilrs/ev/state/struct.GamepadState.html), [custom mappings](https://docs.rs/gilrs/latest/gilrs/struct.Gilrs.html#method.set_mapping), retrieved through Context7. Repository identity: [volsatl9/Connect](https://github.com/volsatl9/Connect); supplied v0.9.0 archive remains the review baseline.
