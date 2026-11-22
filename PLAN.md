# Yeti Studio — Build Plan (v2 — Fable 5.2: APPROVED WITH CHANGES; all 9 required changes applied, see §8)

Cross-platform (macOS + Windows) Rust app that gets clean voice out of an old Blue Yeti
(USB 046d:0ab7, "Yeti Stereo Microphone") for **gaming chat and meetings**.
Only the user's voice should be heard: background noise, fan/wind rumble, mechanical keyboard.

Personal, **unsigned** build (no Apple Developer ID / Windows EV cert available — verified keychain 2026-11-22).

## 1. Facts established on this machine

| Fact | Source |
|---|---|
| macOS 27.2.1, arm64, Xcode installed, Rust 1.100.0, targets aarch64/x86_64-apple-darwin + x86_64-pc-windows-msvc installed | `sw_vers`, `rustup` |
| Yeti enumerates as 2 CoreAudio devices "Yeti Stereo Microphone" (input id, output id), Transport USB | `system_profiler`, CoreAudio probe |
| Settable via CoreAudio: input mute (el 0), input volume scalar/dB per channel (el 1,2; was +14.7 dB), output mute, output volume per channel, nominal rate 44.1k/48k | Swift probe (`kAudioObjectPropertyIsSettable` = true) |
| HID interface: Consumer Control + Telephony input reports only (mute button), no LED output report | `ReportDescriptor` |
| NOT software-controllable: polar-pattern knob, analog gain knob | hardware |
| First plug-in via a hub enumerated without audio interfaces; replug fixed it | observed |

## 2. Decisions (user-approved)

- One codebase, Rust, **egui/eframe** UI.
- UI = 3 surfaces of one app (mockups: https://claude.ai/artifact/8GmzJvRGE3Va3QMgfi4iUL):
  **A Simple** (main window), **B Pro** (same window, "Advanced controls"), **C Mini** (separate always-on-top window, opened from tray/menu bar).
  Pitch-black dark theme, blue accent (#3b9eff dark / #1667d9 light), Light / Dark / System switch.
- Presets: **Gaming** (lowest latency) and **Meeting** (echo cancel, firm auto-level).
- Virtual mic: **macOS** = our own AudioServerPlugIn "Yeti Studio Mic". **Windows** = **VB-CABLE** (option 1, user installs it) for now; output layer abstracted so a SysVAD/APO backend can replace it later.

## 3. Architecture

```
Yeti ─▶ capture (cpal) ─▶ SPSC ring ─▶ DSP thread ─▶ SPSC ring ─▶ output (cpal) ─▶ virtual mic
                                           │                                 (mac: Yeti Studio Mic
                                           ├─▶ meters/waveform (lock-free)     win: CABLE Input)
                                           └─▶ monitor ("hear myself") ─▶ Yeti headphones
system playback ─▶ loopback capture ─▶ AEC far-end reference (Meeting preset)
```

Cargo workspace:

| Crate | Responsibility | Key deps |
|---|---|---|
| `yeti-device` | Find Yeti, get/set input gain, mute, headphone vol/mute, sample rate, hot-plug events. Trait `DeviceControl` + `macos.rs` (CoreAudio via `objc2-core-audio`) + `windows.rs` (`windows` crate: `IMMDeviceEnumerator`, `IAudioEndpointVolume`; sample rate read-only) | objc2-core-audio, windows |
| `yeti-dsp` | Pure, allocation-free-in-process chain: HPF → noise suppression (DeepFilterNet default, RNNoise low-CPU) → VAD gate → EQ + de-esser → auto-level/compressor → limiter. AEC stage (webrtc-audio-processing) for Meeting. Presets (serde). | deep_filter (tract), nnnoiseless, webrtc-audio-processing (bundled), biquad |
| `yeti-engine` | Stream graph, rings, resampling to 48 kHz, device routing, latency + CPU measurement, meter taps, loopback reference capture (mac: Core Audio process tap, macOS 14.2+; win: WASAPI loopback) | cpal, rtrb, rubato |
| `yeti-app` | eframe app: Simple/Pro views, Mini viewport (always on top), tray (`tray-icon`), global hotkeys (`global-hotkey`, keyboard only in v1), theme, config (TOML in OS config dir), first-run tips (disable Krisp/Zoom/Teams suppression) | eframe, tray-icon, global-hotkey, directories, serde, toml |
| `yeti-driver-mac` | AudioServerPlugIn bundle `YetiStudioMic.driver`: 2-ch 48 kHz loopback device; output side hidden-ish ("Yeti Studio Mic (Feed)"), input side "Yeti Studio Mic". Rust cdylib exporting the CFPlugIn factory. **Fallback:** C, based on Apple's NullAudio sample (not BlackHole: GPL-3). | coreaudio-sys (or hand-written FFI) |
| `xtask` | `cargo xtask bundle-mac` (.app + .driver), `install-driver` (prints sudo cmds), `check-windows` (`cargo xwin check`) | — |

Real-time rules (revised): **cpal callbacks are strictly RT-safe** — ring push/pop only, no alloc/lock/syscall/log, enforced with `assert_no_alloc` in debug builds. The **DSP worker** is a separate high-priority thread; bounded allocation inside third-party NS/AEC (tract, WebRTC APM) is tolerated there. Parameters via atomics/triple-buffer. Fixed 10 ms (480-frame) blocks. Underrun → output silence + increment counter.

Clock drift: Yeti USB clock, virtual-device clock and loopback reference are independent. Use a **drift-compensated bridge** (async `rubato` resampler, ratio driven by ring fill level toward a defined target fill) between capture→output and on the AEC reference.

Stereo Yeti → **mono** downmix (L+R)/2 before NS/AEC (DFN is mono); output duplicated to 2 ch.

Dependency notes: DeepFilterNet = **git dep** `Rikorose/DeepFilterNet` `libDF`, `default-features=false, features=["tract","default-model"]` (`default-model-ll` candidate for Gaming) — crates.io `deep_filter` 0.2.5 is unusable. AEC (`webrtc-audio-processing`, bundled, needs meson+ninja) behind cargo feature `aec`, **off for Windows target** until built on real Windows. Loopback: try **cpal 0.18 built-in loopback** first (mac process tap / WASAPI loopback); custom tap via `objc2-core-audio` only if self-exclusion is needed. Driver FFI via `objc2-core-audio` (`AudioServerPlugIn` feature). Pin MSRV + versions, commit `Cargo.lock`.

## 4. Phases & acceptance criteria

| # | Phase | Done when (verifiable) |
|---|---|---|
**User prerequisites (before Phase 0):** `brew install meson ninja`; `cargo install cargo-xwin` (+ accept MSVC CRT/SDK licence on first use); grant microphone permission to the terminal once when prompted (and later "System Audio Recording" for Phase 4).

| # | Phase | Done when (verifiable) |
|---|---|---|
| 0 | Scaffold workspace, pinned versions/MSRV, `Cargo.lock`, xtask, `git init` (local only). **Spike:** DFN (libDF git, tract) builds on rustc 1.100 and renders one WAV | `cargo build --workspace` + `cargo clippy -D warnings` pass on mac; **mandatory** `cargo xwin check --target x86_64-pc-windows-msvc` on pure-Rust crates + `windows.rs` (AEC feature off); DFN spike renders a WAV — if it fails, v1 = RNNoise only (recorded in PLAN) |
| 1 | `yeti-device` + `yeti-engine` passthrough + meters + drift bridge | `yeti-probe` lists Yeti controls and sets/restores gain; passthrough 30 s: cpal `Xrun`=0, engine ring under/overrun counters=0, **captured RMS above threshold while user speaks/taps** (user-assisted, needs mic permission) |
| 2 | `yeti-dsp` chain + offline benchmark | Synthetic tests for HPF (−20 dB @ 30 Hz), limiter (≤ −1 dBFS), gate only. **Fixture tests:** clean speech + recorded keyboard/fan noise at known SNR → noise-only segments ≥ 15 dB reduction, speech segments within 3 dB (or SI-SDR improvement threshold). `yeti-render in.wav out.wav --preset gaming`. **Offline latency + CPU benchmark DFN vs RNNoise** → decides Gaming default |
| 3 | `yeti-app` UI (A/B/C, theme, presets, tray, hotkeys), starts against a **mock engine trait** right after Phase 0 | `egui_kittest` view-logic tests; config round-trip test; Mini window level verified via `CGWindowListCopyWindowInfo` (`kCGWindowLayer`); tray/hotkeys created on main thread after event loop start, handlers call `request_repaint()`; close-to-tray behaviour defined; visual check left to user |
| 4 | Meeting: AEC (feature `aec`) with loopback reference, auto-level | Offline: recorded far-end + near-end mix ≥ 15 dB ERLE. Live (user-assisted, System Audio Recording permission): reference stream RMS > 0 while system audio plays; tap **excludes the app's own process** so monitor isn't fed back |
| 5 | macOS virtual mic driver + app routing. **Timebox Rust driver**; switch to C (Apple NullAudio sample) if the pre-install harness isn't passing by the end of the timebox | **Pre-install (agent-verifiable):** harness loads cdylib, calls CFPlugIn factory, drives `QueryInterface`/`Initialize` (mock host)/property queries/`StartIO`+`DoIOOperation` loopback round-trip in-process; `codesign -s - --verify` + `plutil -lint` on bundle; every FFI entry `catch_unwind` or `panic=abort`; no alloc/locks in `DoIOOperation`; Feed device `IsHidden`/`CanBeDefaultDevice=0`. **Post-install (user runs `sudo` + `sudo killall coreaudiod`):** device appears in `system_profiler`; app → driver → recorded via cpal recorder or ffmpeg shows processed signal |
| 6 | Windows backend (VB-CABLE routing, endpoint volume) | `cargo xwin check` passes; runtime test **deferred to user's Windows PC** (stated, not claimed) |
| 7 | Live latency/CPU + A/B test recorder + **30-min soak** | End-to-end latency (ms) and CPU% for both presets on this Mac replace `[MEASURED]`; 30-min soak: zero ring under/overruns, bounded fill-level drift (user-assisted: Yeti connected) |

## 5. Risks / unknowns (not yet verified)

1. Ad-hoc-signed HAL plug-in loading on macOS 27 — test in phase 5; if blocked, user needs a Developer ID.
2. DeepFilterNet latency may be too high for "Gaming"; fallback: RNNoise in Gaming, DFN in Meeting. Decide from phase 7 numbers.
3. `webrtc-audio-processing` bundled build needs a C++ toolchain (Xcode present on mac; MSVC on Windows).
4. Core Audio process tap API from Rust (objc2) may need custom bindings; needs "Screen & System Audio Recording" permission.
5. Unsigned app: mic (TCC) permission may re-prompt after each rebuild.
6. Windows sample-rate change has no public API — v1 shows it read-only.
7. Driver install requires `sudo` and `sudo killall coreaudiod` (brief audio dropout) — user runs these, agents never do.
8. DeepFilterNet upstream is low-activity; tract version pin (^0.21) may conflict with newer deps.
9. VB-CABLE adds its own buffering latency on Windows.
10. Registered global hotkeys are consumed system-wide (push-to-talk key won't reach the game) — default to a modifier combo, document.
11. Unbundled CLI binaries: mic permission is attributed to the terminal host; silence without error if denied.

## 6. Out of scope for v1

Code signing/notarization, installers, Windows own driver/APO, mouse-button push-to-talk, auto-update, telemetry.

## 7. Execution — agent team

| Agent | Owns | Phase |
|---|---|---|
| Architect | workspace, shared types, xtask, git init | 0 |
| Device & engine engineer | `yeti-device`, `yeti-engine` | 1, 4 (loopback) |
| DSP engineer | `yeti-dsp`, offline renderer, DSP tests | 2, 4 (AEC) |
| UI engineer | `yeti-app` | 3 |
| macOS driver engineer | `yeti-driver-mac`, bundle/install scripts | 5 |
| Windows engineer | windows.rs backends, cross-compile check | 6 |
| QA / reviewer | independent review, runs all acceptance checks, latency/CPU measurement | every phase gate, 7 |

Phases 1, 2, 5 run in parallel after phase 0 (independent crates against shared traits). Phase 3 starts once `yeti-engine` API is stable. Every phase gate = QA agent re-runs acceptance checks; failures go back to owner. Nothing requiring `sudo`, hardware unplugging, or Windows hardware is claimed as verified — listed for the user instead.

## 8. Review log

**2026-11-22 — Fable 5.2: APPROVED WITH CHANGES.** Required changes applied:
1. DFN via libDF git dep + Phase 0 spike (RNNoise-only fallback). 2. meson+ninja prerequisite; AEC behind `aec` feature, off on Windows. 3. cargo-xwin mandatory; gate scoped to pure-Rust + windows.rs. 4. Drift-compensated bridge + 30-min soak. 5. RT rule restated (callbacks strict, DSP worker tolerant), `assert_no_alloc`, underrun = silence + counter. 6. Phase 1 needs captured RMS + engine counters; mic permission noted. 7. Phase 2 uses speech+noise fixtures, synthetic only for HPF/limiter/gate. 8. Phase 5 in-process plugin harness, codesign/plutil checks, FFI panic safety, ffmpeg/cpal recorder, timeboxed Rust→C fallback. 9. Phase 4 live loopback criteria with self-exclusion.
Recommendations adopted: cpal built-in loopback first; objc2-core-audio for driver FFI; hidden Feed device; DFN vs RNNoise benchmark moved to Phase 2; egui_kittest + window-level check; tray/hotkey main-thread + repaint; hotkey consumption risk; mono downmix; pinned versions + Cargo.lock; Phase 3 against mock engine.

**2026-11-22 — Phase 0 DFN spike: PASS (DFN stays in v1).** libDF git `7b14b895` (`default-features=false, features=["tract","default-model"]`) builds on rustc 1.100 **only with tract pinned `=0.21.4`** (resolves to 0.21.19 by default → 17 compile errors) and with tract debug-assertions off in dev builds (model-load check fails otherwise). `spikes/dfn_spike` rendered `data/noisy.wav` (8 s, 48 kHz mono, `say` speech + pink noise) → `data/enhanced.wav`: model load 230 ms, 0.62 ms per 10 ms hop (RTF 0.062, release, M-series), noise-only tail −36.0 → −57.2 dBFS RMS (≈21 dB reduction). Windows cross-check runs with `dfn` off (tract-linalg needs `llvm-lib`); DFN on Windows to be verified on real Windows. DFN state is `!Send` → chain must be built on the DSP thread.
