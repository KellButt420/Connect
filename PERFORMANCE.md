# Video stutter investigation

## Version 0.3 media update

Windows screen sharing now selects an actual Media Foundation hardware H.264 encoder when
available. Low-delay mode, no B frames and bounded in-flight work avoid adding a video queue.
Hardware errors recover on the parallel OpenH264 encoder with a new keyframe. The shelf names
the encoder in use. Capture still reads pixels back to the CPU, converts to I420 and packs
NV12 for the hardware encoder. Video decoding and texture upload still run on the client CPU/GUI.

Host and viewer cursors use independent updates so a static or delayed video frame does not
hold back pointer visibility. GUI input is sent before color conversion and texture upload.
Gamepad state is forwarded for each queued event rather than after draining the entire queue,
preserving quick press/release transitions. Reliable input and host permission checks remain.

Desktop PCM audio has independent bounded queues (20 ms playback target, 80 ms maximum) and
fills missing packets with silence. Muting flushes pending audio. Audio is a separate host
permission, stops on revocation, and restarts its packetizer if the path's datagram limit shrinks.
The preferred 48 kHz stereo float stream costs about 3.1 Mbps before transport overhead.

Local Windows verification on 2026-10-29 used an NVIDIA GeForce RTX 5060. The hardware probe
encoded and decoded 640×480, 1920×1080 and 800×600 frames, recreated the encoder on resize,
checked a forced keyframe with a fresh decoder, and checked timestamp preservation. A 240 Hz
configuration was accepted; this is not a sustained 240 FPS benchmark. Native audio testing
captured a quiet center-channel tone from the 96 kHz/8-channel endpoint into both stereo
channels, with no packet timeline gaps, and reopened capture cleanly. Native cursor testing
checked click-through, focus, capture exclusion, stale hiding, cleanup and GDI handle stability.
A localhost desktop session verified GPU video, cursor delivery, audio enable/revoke and shutdown.
All 79 workspace tests passed, including the opt-in native probes and the rendered shelf/input
isolation check. `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` passed.

Physical two-PC gaming, AMD/Intel hardware transforms, Linux playback and exclusive fullscreen
overlay behavior have not been measured here. The notes below describe the earlier software
optimization and remaining pipeline risks; no zero-delay or universal FPS claim is made.

## Confirmed bottleneck and change

`crates/host/src/encode.rs` previously used OpenH264's default single-slice configuration.
In the locked dependencies (openh264 / openh264-sys2 0.9.9), the Rust wrapper explicitly
selects `SM_SINGLE_SLICE`; OpenH264's `InitSliceSettings` then limits the worker count to
the number of slices. Automatic thread selection therefore still produced one worker.

The encoder now requests size-limited slices (16 KiB) and between one and four workers,
bounded by `available_parallelism()`. This uses the existing safe API and keeps each
encoded frame as one access unit for the existing packetizer and decoder. Multiple slices
can add some compression overhead; the bitrate target and rate-control frame skipping
are preserved. Encoding complexity and resolution are unchanged.

The BGRA-to-YUV conversion already uses the optimized `read_bgra8` path in the locked
version, rather than the older column-major per-pixel conversion. No replacement pixel
converter or new codec dependency is needed.

## Build requirement

OpenH264 silently builds its C++ fallback if NASM is unavailable. Use NASM when building
on x86/x64 so the codec includes its assembly optimizations. The local portable build
tool is `.tools/nasm-2.16.03/nasm.exe`; `.tools` is ignored by Git. No system installation
or persistent PATH change is required.

```powershell
$env:NASM = (Resolve-Path .tools/nasm-2.16.03/nasm.exe).Path
# Only needed if the codec was previously built without NASM:
cargo clean -p openh264-sys2 --release
cargo build --release -p connect --locked
```

For an independent Windows build, obtain NASM from its official distribution and set
`NASM` to the local executable. Linux build prerequisites are in README.md.

## Verification

```text
cargo test -p connect-host --locked
cargo test -p connect-host --lib compare_encoder_frame_times --locked -- --ignored --nocapture --test-threads=1
```

The regression test fails with the old configuration because encoded frames contain
only one video slice. It passes with the change, checks decoding across a resize, and
checks that a requested keyframe can start a fresh decoder. A separate test covers the
16x16 minimum image. End-to-end localhost tests exercise 120 and 240 Hz generated sources,
datagram assembly, decoding, and clean disconnect. These check correctness, not sustained
120/240 decoded frames per second.

The opt-in comparison encodes the same 60 precomputed 1920x1080 moving images with the
original and current configurations. Only this comparison disables frame skipping so
both encoders process every image. It reports median and p95 encode time, skipped frames,
and bytes. Capture, color conversion, network, and presentation are excluded from timing.
The synthetic image is deliberately demanding; its throughput is not a game FPS prediction.

Local results on 2026-10-29, with NASM enabled for both configurations:

| Configuration | Median encode | p95 encode | Skipped / 60 | Encoded bytes |
| --- | ---: | ---: | ---: | ---: |
| Original single slice | 27.89 ms | 29.70 ms | 0 | 9,641,064 |
| Parallel slices | 10.90 ms | 14.11 ms | 0 | 10,022,605 |

This run reduced median encode time by about 61% and p95 by about 52%, with about 4%
more encoded bytes in the unthrottled comparison. Results vary with CPU and content.
The final host suite passed all 19 regular tests; the opt-in comparison also passed.
`cargo build --release -p connect --locked` succeeded, and the executable's `--help`
startup check exited successfully. The native build contains 105 NASM object files.

Initial pre-fork benchmark executable SHA-256 (the v0.2.2 release has its own attached `SHA256SUMS.txt`):
`7F2B4AB0CBBA90505BE57FF9681152411BC7DB915B41CC8D7BBAEE263900D786`.

## Remaining limits

The software fallback, GPU readback, color conversion, the single decoder thread,
and texture upload can also limit high-refresh streaming. The earlier slice change left frame
queues and the wire protocol unchanged. Version 0.3 requires both peers to update. Actual perceived
stutter and input latency must be checked with a game between two PCs.

To try the local build, run `target/release/connect.exe` on the host and reconnect from
another updated client. Toggle stats with Ctrl+Alt+S and compare encode time and visible
motion in the same game scene.

## Follow-up: other concrete stutter paths

The encoder benchmark confirms a CPU bottleneck, but does not establish which stage
caused a particular live-game freeze. Further inspection found:

- **Packet reordering can force recovery without packet loss.** In `FrameAssembler::push`,
  completion of a newer frame immediately abandons older partial frames. A delayed packet
  for an earlier frame then cannot repair the reference chain. A local probe delivered all
  packets for frames 0, 1, 2, and 3, with frame 2 completing before frame 1. The assembler
  reported one lost frame, skipped two complete frames, and remained waiting for a keyframe.
  Reproduction order: deliver keyframe 0; the first packet of frame 1; all of frame 2;
  the rest of frame 1; then all of frame 3. No packets are lost in this sequence.
- **Decoder overload causes a recovery pause.** `client/src/video.rs` queues four encoded
  frames. When full, it drops a reference frame and waits for a keyframe. This is required
  for correctness after that drop, but makes brief overload visible as a freeze. Keyframe
  requests are rate limited to one per 200 ms; requests within the interval are discarded,
  so repeated recovery can extend a pause. A faster sender can expose a slower receiver.
- **GPU readback blocks the entire host video pipeline.** DXGI capture copies to a staging
  texture and immediately maps it for CPU access. That synchronous read can wait for GPU
  work. Capture, conversion, and encoding run sequentially, so any readback stall also
  delays encoding the next frame. Its impact during GPU-heavy games is not measured here.
- **The stats overlay misses important stalls.** Capture time is stamped after DXGI readback;
  encode time excludes capture and color conversion. Encode/decode/latency figures use the
  latest successfully decoded frame, sampled about once a second. They do not describe
  frame interval spikes, presentation delay, or decoder queue overflow counts. A low
  displayed latency therefore does not rule out freezes.

No additional production changes were made for this follow-up. The next diagnostic step
is a real-game trace of capture/readback, conversion, encoding, decoder queue overflow,
keyframe recovery, and presentation intervals. That would distinguish CPU saturation,
GPU stalls, network reordering/loss, and receiver overload before choosing another fix.
