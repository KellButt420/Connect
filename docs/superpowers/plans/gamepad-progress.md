# Progress — 2026-11-03-gamepad-v0.9.1.md

User approved implementation, USB only. Future performance work stays documented, outside this release.

- Ruling: work in the supplied source archive; no Git metadata exists, so worktree/commit scripts cannot apply.
- Ruling: use inline implementation and self-review; the approved plan says no delegation is needed.
- Ruling: hardware reproduction remains pending because no failing Linux controller is attached to this Windows environment. Implement synthetic A→X and LB→LT regression coverage and expose diagnostics for hardware verification.
- Preflight: mapping types are shared by Settings, worker and UI; one module owns their validation and report conversion. UI drafts never enter saved Settings before Apply.

## Status

- Mapping/profiles: implemented; 7 mapping/capture tests and 2 routing tests pass in the focused harness, including A→X/LB→LT reassignment, trigger ranges, conflict handling and axis-direction ambiguity.
- Discovery/forwarding/UI: implemented; App-owned worker, Settings tester/editor, saved profiles, and lifecycle wiring compile. Final app suite: 21 passed, 1 manual UI test ignored.
- Validation/builds/roadmap: final Windows MSVC release build passed with portable NASM; `connect.exe --help` launches successfully. Strict app Clippy passed. Linux/controller validation is delegated to the user at their explicit request. Roadmap saved in `docs/PERFORMANCE-ROADMAP.md`.

## Execution findings

- Added a small `routing.rs` module to test real slot/edge/lifecycle behavior without constructing a hardware backend. It owns session-local report delivery, not OS input.
- Profiles store independently encoded RON records inside existing Settings. Invalid records produce visible errors without discarding unrelated preferences.
- Rebinding reports source conflicts explicitly before clearing old routes. Capture tokens prevent cancelled/replaced captures from being applied later.
- The Windows MSVC toolchain is used because the documented Linux/MinGW build environment is absent. Portable NASM 2.16.03 is project-local; no system installation or configuration change.
- Initial parallel host tests timed out on `allow_streams_video` and `high_refresh_video_decodes_over_datagrams`; each passed individually. The new gamepad transport/permission/disconnect test passed.
- Earlier workspace runs hit mixed-media/video timeouts while builds were running. The final serial workspace run passed every test, including the unchanged mixed-media test: 162 passed, 10 hardware/manual tests ignored. No video fix was needed or included.
- Sandboxed native build/lint checks failed in MSBuild FileTracker with `E_ACCESSDENIED`; both passed when rerun outside the sandbox.
- Restored 66 unrelated formatting-only files from the original ZIP after verifying each was equivalent to its rustfmt output. Preserved surrounding style in the five integration files after proving their canonical Rust source was identical.
- Review fix: reject both directions of one axis in a single capture attempt; watched its test fail then pass.
- Review fix: apply the same gilrs filters explicitly once per backend event, so continuous filtered jitter cannot starve lifecycle commands. Preserve a single state update per event.

## Final checks and handoff

- Workspace: `cargo test --locked --offline --workspace --no-fail-fast -- --test-threads=1` — 162 passed, 10 ignored, zero failures. This serial run avoids competing high-refresh test streams.
- Final app check after controller-worker/UI refinements: `cargo test --locked --offline -j 4 -p connect --bin connect` — 21 passed, 1 ignored.
- Lint: `cargo clippy --locked --offline -j 4 -p connect --bin connect --no-deps -- -D warnings` — passed outside the sandbox.
- Formatting: new controller modules pass `rustfmt --check`; integration files retain surrounding repository style. Unrelated files retain their original formatting.
- Build: `cargo build --locked --offline --release -j 4 -p connect --target-dir target/release-build` with NASM configured — passed. CLI `--help` smoke test passed.
- Protocol remains 8. No dependency versions changed beyond the seven workspace packages moving from 0.9.0 to 0.9.1.
- Final review: self-review, per the approved no-delegation plan. Reviewed source identities, capture cancellation/ambiguity, profile validation, preview/forwarding consistency, permissions, focus, stable slots and shutdown. Physical USB and GUI interaction acceptance remain for user testing.
- Not executed here: Linux compilation, physical Linux/Windows controller matrix, two-version peer interoperability or input-to-photon comparisons. The user explicitly requested source push and will test on their separate Linux system.
- Publishing target: `KellButt420/Connect`, branch `v0.9.1-gamepads`. Its main was v0.3.0; the branch first imports the supplied upstream v0.9.0 archive, then adds this feature. Main is not overwritten and no release is published.
