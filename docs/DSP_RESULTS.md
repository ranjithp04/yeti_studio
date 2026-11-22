# Yeti Studio — DSP results (Phase 2 + Phase 4 AEC, offline)

Owner: DSP engineer · Measured 2026-11-22 on this Mac (Apple M3 Max, macOS 27.2.1, rustc 1.100,
release build with `debug = 1`, `lto = "thin"`). Everything here is **offline** (WAV in → WAV out);
live end-to-end latency/CPU is Phase 7.

Reproduce:

```sh
cargo run --release -p yeti-dsp --features aec --bin yeti-dsp-bench [-- --wav-dir /some/dir]
cargo test  -p yeti-dsp --features aec            # unit + fixture + AEC tests
cargo run --release -p yeti-dsp --features aec --bin yeti-render -- in.wav out.wav --preset gaming [--ns dfn|rnnoise]
```

## Chain

`(AEC3, Meeting) → rumble HPF (4th-order Butterworth, 80 Hz) → noise suppression (DeepFilterNet3 | RNNoise)
→ voice gate (level + NS-model VAD with 250 ms hang-over) → EQ (200 Hz shelf, 3 kHz presence, 10 kHz air)
+ dynamic-shelf de-esser → auto-level (slow AGC + RMS compressor) → 1 ms look-ahead brick-wall limiter (−1 dBFS)`.

Algorithmic latency (48 kHz frames): DFN 1440 (30 ms) · RNNoise 480 (10 ms) · limiter 48 (1 ms) · AEC 430 (9 ms,
the WebRTC APM band-split/framing delay, measured with broadband noise). These are reported by
`VoiceChain::latency_frames()` and checked against cross-correlation in tests.

## Fixture limitation (read this first)

No recorded speech, keyboard or fan clips were available offline, so:

- **Speech:** primary fixture = macOS `say` TTS (12.9 s, three sentences with pauses,
  `crates/yeti-dsp/tests/fixtures/speech_tts.wav`). Real phonemes, but a synthetic voice and no room.
  Secondary = a fully synthetic "speech-like" signal (glottal pulses → moving formants, syllable
  envelopes, fricatives). **DeepFilterNet does not treat the synthetic signal as speech** (it removes
  ~15 dB of it), so the synthetic rows measure "how the model handles a non-speech harmonic signal",
  not speech quality. Acceptance tests use the TTS fixture only.
- **Noise:** synthesised mechanical keyboard (press/release clicks: broadband transient + plate/body
  resonances, bursty typing) at −8 dB vs. a fan (low-passed pink noise + 110 Hz blade tones + < 60 Hz rumble).
  Mixed at **5 dB SNR** (speech-active RMS vs. noise RMS).
- Real Yeti recordings (room reverb, real switches, real fan) may score differently → user step:
  record a real take and run `yeti-render` / the A/B test.

## Phase 2 acceptance (TTS fixture, `tests/fixture_quality.rs`)

| Criterion (PLAN §4) | DFN | RNNoise | Result |
|---|---|---|---|
| HPF −20 dB @ 30 Hz (synthetic) | — | — | ≈ −34 dB analytic, ≤ −20 dB measured on a 30 Hz sine — pass |
| Limiter ≤ −1 dBFS (synthetic +6 dBFS sine, square bursts, 8.0 spikes) | — | — | pass (peak ≤ ceiling) |
| Gate (synthetic): −20 dBFS tone passes ±0.5 dB, −66 dBFS noise attenuated > 28 dB | — | — | pass |
| Noise-only segments ≥ 15 dB reduction (NS-only chain) | 87.3 dB | 25.4 dB | pass |
| Speech segments within 3 dB (NS-only chain) | −0.5 dB | −0.3 dB | pass |
| SI-SDR improvement ≥ 10 dB (NS-only chain) | −6.4 → 12.6 (+19.0) | −6.4 → 8.7 (+15.1) | pass |
| Full Gaming / Meeting presets: noise-only ≥ 15 dB, peak ≤ −1 dBFS | 87.1 / 61.8 dB | 21.3 / 20.1 dB | pass |
| Phase 4 offline: AEC ERLE ≥ 15 dB | 49.7–49.8 dB (TTS far end), 43.9–48.8 dB (synthetic) | | pass |

"NS-only chain" = HPF + NS + gate (EQ/auto-level/limiter off) so level changes are attributable to noise
removal. Noise-only frames exclude ±200 ms around speech (gate release, STFT smearing). The preset rows
above are with `aec` on; without `aec` (Meeting = Gaming + firmer auto-level, longer gate release) the
Meeting preset gives 86.9 dB (DFN) / 19.5 dB (RNNoise) noise-only reduction.

## Benchmark output (`yeti-dsp-bench`, release; verbatim except headings demoted)

Build: release; DFN compiled: true; AEC compiled: true; noise SNR 5 dB (speech-active RMS vs noise RMS); noise-only frames exclude ±200 ms around speech.

DFN model load: 217 ms

VoiceChain::new (all models): 202 ms

### TTS speech + keyboard/fan

SI-SDR reference = clean speech through the same rumble filter; it is only meaningful for the NS-only rows (EQ / auto-level / AEC band-splitting change the waveform on purpose).

| Config | Latency reported / measured (ms) | Block time mean / p99 / max (µs) | CPU (1 core) | Noise-only reduction (dB) | Speech level Δ (dB) | SI-SDR in → out (dB) |
|---|---|---|---|---|---|---|
| DFN NS-only (HPF+NS+gate) | 31.0 / 30.8 | 509 / 967 / 1219 | 5.1 % | 87.3 | -0.5 | -6.4 → 12.6 |
| RNNoise NS-only (HPF+NS+gate) | 11.0 / 10.8 | 63 / 79 / 142 | 0.6 % | 25.4 | -0.3 | -6.4 → 8.7 |
| DFN Gaming preset | 31.0 / 30.8 | 548 / 1187 / 1387 | 5.5 % | 87.1 | +1.0 | -6.4 → 11.3 |
| DFN Meeting preset | 40.0 / 34.3 | 660 / 1154 / 1423 | 6.6 % | 61.8 | -1.3 | -6.4 → 1.9 |
| RNNoise Gaming preset | 11.0 / 10.8 | 73 / 92 / 147 | 0.7 % | 21.3 | +1.2 | -6.4 → 7.8 |
| RNNoise Meeting preset | 20.0 / 22.6 | 199 / 300 / 355 | 2.0 % | 20.1 | -1.2 | -6.4 → -0.4 |

(752 speech frames, 354 noise-only frames of 10 ms)

### synthetic speech + keyboard/fan

SI-SDR reference = clean speech through the same rumble filter; it is only meaningful for the NS-only rows (EQ / auto-level / AEC band-splitting change the waveform on purpose).

| Config | Latency reported / measured (ms) | Block time mean / p99 / max (µs) | CPU (1 core) | Noise-only reduction (dB) | Speech level Δ (dB) | SI-SDR in → out (dB) |
|---|---|---|---|---|---|---|
| DFN NS-only (HPF+NS+gate) | 31.0 / 31.0 | 397 / 950 / 1516 | 4.0 % | 55.5 | -15.8 | 2.8 → -9.8 |
| RNNoise NS-only (HPF+NS+gate) | 11.0 / 11.0 | 62 / 76 / 80 | 0.6 % | 25.0 | -3.7 | 2.8 → 1.3 |
| DFN Gaming preset | 31.0 / 31.0 | 402 / 926 / 1228 | 4.0 % | 46.8 | -7.3 | 2.8 → -12.5 |
| DFN Meeting preset | 40.0 / 40.0 | 520 / 1069 / 1343 | 5.2 % | 41.4 | -7.7 | 2.8 → -11.2 |
| RNNoise Gaming preset | 11.0 / 11.0 | 74 / 96 / 114 | 0.7 % | 17.1 | -2.9 | 2.8 → -1.0 |
| RNNoise Meeting preset | 20.0 / 20.0 | 203 / 258 / 286 | 2.0 % | 11.3 | -4.8 | 2.8 → -2.7 |

(729 speech frames, 258 noise-only frames of 10 ms)

### Per-stage CPU (`TTS speech + keyboard/fan`)

| Stage | mean (µs / 10 ms block) | p99 (µs) | CPU (1 core) |
|---|---|---|---|
| HPF (4th-order) | 2.4 | 3.4 | 0.02 % |
| RNNoise | 36.2 | 50.7 | 0.36 % |
| DeepFilterNet3 | 478.7 | 888.5 | 4.79 % |
| Voice gate | 2.4 | 3.3 | 0.02 % |
| EQ + de-esser | 7.4 | 14.6 | 0.07 % |
| Auto-level | 4.0 | 5.1 | 0.04 % |
| Limiter | 17.8 | 23.2 | 0.18 % |
| AEC3 (no reference) | 128.8 | 173.1 | 1.29 % |

### AEC (WebRTC AEC3) offline ERLE

Far end = speech fixture played through a synthetic room (pure delay + exponentially decaying noise IR, RT60 150 ms, −6 dB). Near mic = echo + −70 dBFS white noise (single talk). ERLE = 10·log10(E[mic] / E[AEC out]) over frames where the echo is within 35 dB of its peak, excluding the first 3 s (convergence). Double talk: a second (synthetic) talker at −23 dBFS is added to the mic; Δ = its level in the AEC output vs. clean, aligned by the APM's 430-frame delay.

| Far-end speech | Echo delay (ms) | ERLE after 3 s (dB) | Double-talk near-speech Δ (dB) |
|---|---|---|---|
| TTS speech | 20 | 49.7 | -6.8 |
| TTS speech | 60 | 49.8 | -5.7 |
| synthetic speech | 20 | 43.9 | -2.9 |
| synthetic speech | 60 | 48.8 | -2.8 |

## Reading the numbers

- **Latency** matches the reported value to within 0.2 ms on both fixtures (TTS rows show 30.8 vs 31.0:
  cross-correlation on voiced speech plus the HPF's phase shift). Meeting rows on TTS (34.3 / 22.6 ms
  measured vs 40 / 20 reported) are pitch-period ambiguity of speech cross-correlation through AEC; the
  AEC delay itself (430 frames) is verified with broadband noise in `aec::tests`.
- **CPU:** DFN ≈ 0.48 ms mean / 0.9–1.2 ms p99 per 10 ms block (≈ 5 % of one M3 Max core, real-time
  factor ≈ 0.05); RNNoise ≈ 0.036 ms (0.4 %). Everything else in the chain together ≈ 0.04 ms,
  AEC3 ≈ 0.13 ms. Worst single block seen across all runs: ≈ 2.3 ms (DFN), i.e. > 4× headroom in a 10 ms
  period. Model load (DFN) ≈ 0.22 s, done once in `VoiceChain::new`.
- **Quality (TTS):** DFN removes noise-only segments completely (the gate closes and DFN outputs digital
  silence → 87 dB) and gives +19 dB SI-SDR; RNNoise gives +15 dB SI-SDR and ~21–25 dB in noise-only
  segments. SI-SDR includes the keyboard clicks *under* speech, where the gate cannot help: this is where
  DFN's advantage matters for the "mechanical keyboard" goal.
- **Gate:** keyed by level *and* the NS model's VAD (2-frame smoothed, > 0.8, 250 ms hang-over).
  RNNoise's VAD spikes to 0.6–0.8 on keyboard bursts; without the sustained-VAD rule the RNNoise Gaming
  preset only reached 12–16 dB in noise-only segments. DFN's lsnr (≈ −15 dB on noise, −10…+9 dB on speech)
  is mapped to a probability by a logistic centred at −7 dB (`DfnSuppressor::voice_probability`).
- **AEC:** ERLE ≈ 44–50 dB after convergence on a synthetic room (single talk). During double talk
  AEC3's suppressor ducks the near talker by 3–7 dB. Live behaviour with a real loopback reference and
  clock drift is Phase 4 live / Phase 7 (user-assisted).
- **Synthetic speech fixture:** DFN attenuates it by 15 dB (NS-only) because it is not real speech; RNNoise
  by 3.7 dB. This is a fixture limitation, not a chain bug — but it also means **real voices that DFN
  finds unusual (whispering, singing, shouting in-game) should be listened to** before trusting DFN blindly.

## Recommendation: Gaming default = **DeepFilterNet** (RNNoise as the one-click low-latency option)

| | DFN (Gaming) | RNNoise (Gaming) |
|---|---|---|
| DSP algorithmic latency | 31 ms | 11 ms |
| CPU (1 core, M3 Max) | ≈ 5 % | ≈ 0.7 % |
| Noise-only reduction (TTS + keyboard/fan, 5 dB SNR) | 87 dB | 21 dB |
| SI-SDR gain (NS-only) | +19.0 dB | +15.1 dB |

Why DFN: the stated goal is "only my voice — no fan, no mechanical keyboard", and keyboard clicks *during*
speech are exactly where RNNoise is weakest (−4 dB SI-SDR vs DFN). 20 ms extra is small next to typical
voice-chat paths (Opus 20 ms frames + jitter buffer + network ≫ 50 ms) and CPU is ≈ 5 % of one core.
Gaming remains the lowest-latency preset (no AEC: 31 ms vs 40 ms for Meeting).

Why not unconditionally: if the user is latency-sensitive (hearing themselves via "Hear myself", or on a
slow/older CPU, or on Windows where DFN is not yet verified — `dfn` is off in the Windows check), RNNoise
is the right choice; the UI exposes it on the AI-noise-removal card. **This matches the existing
`yeti-types` defaults (`NoiseModel::DeepFilterNet` for both presets), so no shared-type change was made.**
Revisit with Phase 7 live end-to-end numbers; `default-model-ll` (DFN low-latency model) is an untested
candidate to cut the 30 ms.

## Known issues / not verified

- All numbers are offline on synthetic/TTS fixtures; no real Yeti recording was processed (needs the user).
- DFN outputs exact digital silence after ~5 near-silent frames (libDF behaviour); fine for a virtual mic,
  but some apps' "is the mic working?" indicators may show nothing during pauses.
- `RnnSuppressor::reset()` rebuilds its `DenoiseState` (small allocation) — only on model switch/reset,
  on the DSP worker (tolerated per PLAN §3), never in a cpal callback.
- AEC on Windows: feature off (not built on Windows).
- Listening quality (musical noise, word-onset clipping by the gate) was not judged by ear.
