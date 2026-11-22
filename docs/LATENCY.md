# Live end-to-end latency and CPU (Phase 7)

Measured 2026-11-22 on this Mac (macOS 27.2.1, arm64) with the real Blue Yeti
("Yeti Stereo Microphone", 48 kHz, stereo → mono). Release build of `yeti-passthrough`
with `--features aec` (same features as the bundled app). Every run below had all counters at zero:
cpal xruns, capture overruns, output underruns/overruns, ring resyncs and reference underruns.

```sh
cargo build -p yeti-engine --release --features aec --bin yeti-passthrough
./target/release/yeti-passthrough --seconds 30 --probe --preset gaming|meeting [--ns dfn|rnnoise] [--buffer N]
```

## Method (`--probe`, `crates/yeti-engine/src/probe.rs`)

- The capture callback replaces the mic signal with silence plus one full-scale impulse every
  250 ms. It records the impulse's capture time: cpal `InputStreamTimestamp::capture` plus the
  frame offset.
- The DSP worker still runs the full voice chain, so its CPU time and scheduling are part of the
  path. It then forwards the raw block, because NS and the gate would remove an impulse.
- The main output callback finds the impulse and records `OutputStreamTimestamp::playback`
  plus the frame offset. It then plays **silence**.
- Both timestamps are on the CoreAudio host clock. They include the device buffer, safety offset
  and device latency that the HAL reports. **Transport** = playback − capture, averaged over
  about 120 impulses per 30 s run.
- **End-to-end** = transport + the voice chain's algorithmic delay: DFN 30 ms, RNNoise 10 ms,
  AEC 9 ms, limiter 1 ms. These delays were verified by offline cross-correlation in
  `docs/DSP_RESULTS.md` and are reported by `VoiceChain::latency_ms()`.
- **CPU** = the DSP worker's busy time per 10 ms block, as a % of one core (moving average,
  value at the end of the run). It covers the chain plus the input and output resamplers.

## Results (default buffer 128 frames, output = Yeti headphone jack, 30 s each)

| Preset | NS model | Transport mean (min–max) | Chain delay | **End-to-end** | CPU (1 core) |
|---|---|---|---|---|---|
| Gaming | DeepFilterNet (default) | 35.4 ms (35.1–37.8) | 31.0 ms | **66.4 ms** | 3.8 % |
| Gaming | RNNoise | 35.9 ms (35.3–39.6) | 11.0 ms | **46.9 ms** | 1.0 % |
| Meeting (AEC on, self-excluding tap) | DeepFilterNet (default) | 37.2 ms (37.0–37.3) | 40.0 ms | **77.2 ms** | 5.3 % |
| Meeting (AEC on, self-excluding tap) | RNNoise | 36.1 ms (35.9–37.1) | 20.0 ms | **56.0 ms** | 2.6 % |

Effect of buffer size (Gaming, DFN; 15 s runs unless noted):

| Buffer (frames) | Transport | End-to-end | Counters |
|---|---|---|---|
| 512 (device default, used before this fix) | 83.6 ms | 114.6 ms | 0 |
| 256 | 48.6 ms | 79.6 ms | 0 |
| **128 (new macOS default, `DEFAULT_BUFFER_FRAMES`)** | 35.8 ms (120 s run, n=479) | 66.8 ms | 0 |

Meeting at 512 frames: 84.7 ms transport, 124.6 ms end-to-end.

## Caveats

- The output here is the **Yeti headphone jack**, not the virtual mic, because the
  "Yeti Studio Mic" driver is not installed. Through the driver, the transport adds the Feed
  device's buffer and safety offset, and the receiving app adds its own input buffer. Re-run with
  `--output "Yeti Studio Mic (Feed)"` after `cargo xtask install-driver`.
- Device latencies come from the HAL's reported values, not from an acoustic loopback.
- The CPU figures were taken with the probe's impulse input, not live speech. Per-block compute
  of DFN, RNNoise and AEC does not depend on the signal: the offline benchmark gives the same
  order (`docs/DSP_RESULTS.md`).
- A 30-minute soak (PLAN §4 Phase 7) has **not** been run yet. Longest run so far: 120 s, all
  counters zero, output fill 748–781 frames against a 768 target in a 60 s live-mic run.

## Gaming default

DFN stays the Gaming default, as recommended in `docs/DSP_RESULTS.md`: it is much better on
keyboard noise under speech. At about 66 ms, Gaming/DFN is still the lowest-latency DFN
configuration. If latency matters more than keyboard suppression, choosing RNNoise on the
AI-noise-removal card saves 20 ms (about 47 ms end to end).
