# Yeti Studio — cross-crate API contracts (Phase 0)

Source of truth is the Rust code; this file summarises it and records ownership.
Shared-file changes (workspace `Cargo.toml`, `crates/yeti-types`) must be **minimal and additive**
and noted in your report.

## Ownership map

| Crate / file | Owner | Notes |
|---|---|---|
| `Cargo.toml` (workspace), `Cargo.lock`, `.cargo/`, `.gitignore`, `docs/API.md` | Architect | Others: additive edits only (e.g. move a pin out of the PLANNED block) |
| `crates/yeti-types/**` | Architect | Additive changes only (`#[serde(default)]` on new fields) |
| `crates/yeti-device/src/{lib.rs,mock.rs,macos.rs}` | Device & engine engineer | |
| `crates/yeti-device/src/windows.rs` | Windows engineer | Module shadows the `windows` crate: use `::windows::...` |
| `crates/yeti-engine/**` | Device & engine engineer | `mock.rs` may be extended by UI engineer on request |
| `crates/yeti-dsp/**` | DSP engineer | |
| `crates/yeti-app/**` | UI engineer | |
| `crates/yeti-driver-mac/**` | macOS driver engineer | |
| `xtask/**` | Architect (skeleton); `bundle-mac` / `install-driver` bodies: macOS driver engineer; `check-windows`: Windows engineer | |
| `spikes/dfn_spike/**` | Architect (Phase 0 spike, not a workspace member) | |

## Conventions (`yeti-types`)

- Engine audio: **48 kHz (`ENGINE_SAMPLE_RATE`), mono, `f32`, 480-frame blocks (`BLOCK_FRAMES`)**.
- Levels in dBFS; meters floor at `METER_FLOOR_DB` (−90).
- Errors: `yeti_types::YetiError` / `yeti_types::Result<T>` everywhere at crate boundaries.

### Shared types
| Type | Purpose |
|---|---|
| `Theme` | Light / Dark / System |
| `Preset` | Gaming / Meeting / Custom; `DspParams::for_preset(p)` gives the parameters |
| `ViewMode` | Simple / Pro / Mini |
| `DspParams` | One sub-struct per voice-chain card: `high_pass` (Rumble filter), `noise` (AI noise removal: `model` DFN/RNNoise, `strength` 0..1 = noise-strength slider), `gate` (Voice gate), `eq` (Voice EQ + de-esser), `auto_level`, `echo_cancel`, `limiter`; plus `output_muted` (software mute) |
| `DeviceState`, `DeviceCommand`, `DeviceEvent` | Yeti hardware state / setters / hot-plug |
| `MeterSnapshot` | input/output peak+RMS, noise reduction, gain change, gate open, voice probability |
| `WaveformSnapshot` | raw vs cleaned peak buckets (oldest first, equal length) + `bucket_ms` |
| `EngineStats`, `EngineCounters` | latency (e2e + DSP), CPU %, drift ratio, xruns / overruns / underruns / reference underruns / blocks |
| `EngineStatus`, `EngineEvent` | lifecycle + async notifications to the UI |
| `AbState`, `AbVariant` | A/B test recorder |
| `MonitorSettings` | "Hear myself" enable + volume |

## `yeti-device::DeviceControl` (trait, `Send + Sync`)

`state()`, `set_input_gain_db`, `set_input_muted`, `set_headphone_volume`, `set_headphone_muted`,
`set_sample_rate` (Windows → `Unsupported`), `set_event_callback(Option<Arc<dyn Fn(DeviceEvent)>>)`,
provided `apply(DeviceCommand)`.
- `open_yeti() -> Result<Box<dyn DeviceControl>>` picks `macos::CoreAudioYeti` / `windows::WasapiYeti` (stubs → `NotImplemented`).
- `watch_yeti()` (additive): like `open_yeti` but on macOS succeeds while the Yeti is unplugged
  (`state().connected == false`) and fires `Connected` on plug-in; other platforms = `open_yeti`.
  macOS backend implemented (Phase 1); `macos::list_devices()` for diagnostics.
- `MockDevice` = working in-memory implementation (+ `set_connected` to simulate hot-plug).
- Setters validate and return `OutOfRange`; never called from audio callbacks.
- Identification constants: `YETI_NAME`, `YETI_USB_VID` (0x046d), `YETI_USB_PID` (0x0ab7).
- Windows (Phase 6): `windows::WasapiYeti` (endpoint volume/mute, rate read-only), helpers
  `list_endpoints`, `find_vb_cable_input`, `vb_cable_input`, `default_render_endpoint`;
  platform-free `endpoint_names` (Yeti / VB-CABLE name matching, `VB_CABLE_MISSING_MESSAGE`,
  `VB_CABLE_DOWNLOAD_URL`). Engine side: `yeti_engine::windows_routing` (`virtual_mic_output`,
  `yeti_capture_device`, `yeti_headphone_device`, `start_loopback_reference`,
  `reference_includes_monitor`). Not yet run on Windows.

## `yeti-dsp::Processor` (trait) and `VoiceChain`

```rust
pub trait Processor {
    fn name(&self) -> &'static str;
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()>;
    fn process(&mut self, block: &mut [f32]);   // mono, in place
    fn reset(&mut self);
    fn latency_frames(&self) -> usize { 0 }
}
```
- **Not `Send`**: DFN (tract) state contains `Rc`, so `DfnSuppressor` and `VoiceChain` (with `dfn`) are `!Send`.
  **The engine must construct `VoiceChain` on the DSP worker thread.**
- `VoiceChain::new(DspParams)`, `set_params(&DspParams)` (cheap, between blocks),
  `process_block(near, far_end: Option<&[f32]>) -> ChainMetrics`, `latency_ms()`,
  `aec_available()`, `dfn_available()`. Current stub = passthrough + output mute.
- `dfn::DfnSuppressor` (feature `dfn`): working DFN3 wrapper (`new`, `set_strength`, `last_lsnr_db`).
- Features: `dfn` (default) and `aec` (empty until Phase 4 adds `webrtc-audio-processing`).
- **Phase 2/4 update (DSP engineer):** the chain is implemented (AEC → HPF → NS → gate → EQ/de-esser →
  auto-level → limiter). `aec` now pulls `webrtc-audio-processing` (bundled). Additions:
  `latency_frames()`, `active_noise_model() -> ActiveNs` (DFN falls back to RNNoise if unavailable),
  `dfn_error()`, `aec_error()`, `echo_cancel_active()`. `VoiceChain::new` loads both NS models
  (~0.2 s) and the APM, so `set_params` never reloads. NS/AEC run only on full 480-frame blocks.
  Latency (48 kHz frames): DFN 1440, RNNoise 480, limiter 48, AEC 430 (when echo cancel on).
  `ChainMetrics.voice_probability` comes from the NS model (RNNoise VAD / DFN lsnr heuristic).
  Offline tools: `yeti-render`, `yeti-dsp-bench`; results in `docs/DSP_RESULTS.md`.

## `yeti-engine::Engine` (trait, `Send + Sync`, object-safe; UI holds `Arc<dyn Engine>`)

| Group | Methods |
|---|---|
| lifecycle | `start(EngineConfig)`, `stop()`, `status()` |
| voice chain | `set_params`, `params`, `set_preset`, `preset`, `set_muted`, `is_muted` |
| monitor | `set_monitor(MonitorSettings)`, `monitor()` |
| telemetry (per frame) | `meters() -> MeterSnapshot`, `waveform(&mut WaveformSnapshot)`, `stats() -> EngineStats`, `poll_events() -> Vec<EngineEvent>` |
| device | `device_state()`, `device_command(DeviceCommand)` |
| A/B test | `ab_record(secs)`, `ab_play(AbVariant)`, `ab_stop()`, `ab_state()` |
| capability | `aec_available()` |

All methods `&self`, non-blocking, cheap. `EngineConfig { input_device, output_device, monitor_device }` (`None` = auto).
- `MockEngine`: full fake (time-varying synthetic meters/waveform, presets → Custom detection,
  A/B timing, `simulate_connected(bool)` for hot-plug, backed by `MockDevice`). Use it for Phase 3.
- `YetiEngine` (Phase 1, device & engine engineer): real cpal engine. `start` is non-blocking
  (queues to a supervisor thread; watch `status()`/`poll_events()`: `Starting` → `Running` /
  `WaitingForDevice` / `Error`). Hot-plug: Yeti unplug → `WaitingForDevice`, replug → restarts.
  A/B test (`ab.rs`): `ab_record(0.5..=60 s)` records raw + cleaned (pre-mute) on the DSP worker
  (buffers allocated by the caller, worker uses `try_lock`); `ab_play` plays a take through the
  monitor output (Yeti headphones; error if no monitor device), replacing the live monitor signal;
  `EngineEvent::AbReady` when a take finishes. Requires `Running`.
  - `EngineConfig` (additive fields): `buffer_frames: Option<u32>`, `capture_reference: Option<bool>`
    (`None` = auto: echo cancel on **and** `aec` built). Device names are substring matches;
    `output_device`/`monitor_device` accept `"default"` (system output) and `"none"`.
    `output_device: None` = virtual mic. macOS: "Yeti Studio Mic (Feed)" by name, else by UID
    (`mac_feed::FEED_DEVICE_UID`) wrapped in a private aggregate (`mac_feed::DeviceAlias`) since
    the Feed is hidden; if the driver is absent → **system default output** + `EngineEvent::Warning`
    (Integrator decision; feedback risk on speakers is stated in the warning). Windows:
    `windows_routing::virtual_mic_output` ("CABLE Input"); if absent → no main output + `Warning`
    with `endpoint_names::VB_CABLE_MISSING_MESSAGE`. `input_device: Some("test-tone")`
    = synthetic 440 Hz source (no mic/TCC), for tests.
  - Extras on the concrete type: `diagnostics() -> EngineDiagnostics` (pipeline info, ring fills,
    output overruns/resyncs, monitor/reference drift, reference level, `reference_excludes_self`),
    `set_bypass_chain(bool)`.
  - Telemetry: waveform = 200 buckets × 20 ms; meters are per 10 ms block.
  - Bins: `yeti-probe [--list]`, `yeti-passthrough [--seconds N] [--input NAME|test-tone]
    [--output NAME|yeti|default|none] [--audible] [--monitor] [--bypass] [--reference] [--buffer N]`.
  - Apps embedding the engine may install `assert_no_alloc::AllocDisabler` as global allocator in
    debug builds; cpal callbacks are wrapped in `assert_no_alloc`.

## `yeti-app`

lib (`yeti_app`) + bin `yeti-studio`. Default engine = `YetiEngine`; `--mock` = `MockEngine`
(UI development, tests use it). Flags: `--mock`, `--mini`, `--hidden`, `--quit-after SECS`,
`--config PATH`, `--no-tray`, `--no-hotkeys`, `--input NAME|test-tone`,
`--output NAME|default|none`, `--monitor NAME|default|none` (→ `EngineConfig`).
On macOS the app enables `aec` through a target-specific dependency (Meeting preset echo cancel);
Windows builds keep it off.

## `yeti-driver-mac`

cdylib + rlib. Exports `YetiStudioMic_Create(CFAllocatorRef, CFUUIDRef) -> *mut c_void` (CFPlugIn factory,
Phase 5: implemented, hand-written FFI, no deps).
Constants `MIC_DEVICE_NAME` = "Yeti Studio Mic", `FEED_DEVICE_NAME` = "Yeti Studio Mic (Feed)", `BUNDLE_ID`,
`MIC_DEVICE_UID` = "YetiStudioMic_UID", `FEED_DEVICE_UID` = "YetiStudioMicFeed_UID", `FACTORY_UUID`.
Empty on non-macOS; excluded from Windows checks.
- Format: 2 ch, 48 kHz, f32 interleaved, fixed. Feed (output) → Mic (input) loopback, ~2 IO buffers latency.
- **Engine note:** the Feed device is `IsHidden = 1`, so it is NOT in `kAudioHardwarePropertyDevices`
  (cpal's device list will not show it). Resolve it with `kAudioHardwarePropertyTranslateUIDToDevice`
  + `FEED_DEVICE_UID`, then open that `AudioDeviceID` for output.

## Features & build matrix

| Feature | Crates | Default | Windows check |
|---|---|---|---|
| `dfn` | yeti-dsp → yeti-engine → yeti-app | on | **off** (tract-linalg needs `llvm-lib`, absent on this Mac) |
| `aec` | same chain | off (yeti-app on macOS: **on**) | off |

## xtask

- `cargo xtask check-windows [--with-dfn]` — `cargo xwin check --target x86_64-pc-windows-msvc --no-default-features` on all crates except `yeti-driver-mac`.
- `cargo xtask bundle-mac [--driver-only]` — release build, assemble + ad-hoc sign + `codesign --verify` + `plutil -lint`
  `dist/YetiStudioMic.driver` (then runs the driver harness on the signed bundle) and `dist/Yeti Studio.app`.
- `cargo xtask install-driver` — prints sudo install/uninstall commands only.

## Gotchas found in Phase 0

1. libDF does not compile against tract 0.21.19 → tract pinned `=0.21.4` (workspace).
2. tract 0.21.4 debug-assertions reject the DFN3 model ("duplicate name /convt3/Conv.bias") →
   `debug-assertions = false` for tract-*/deep_filter in `[profile.dev.package]`.
3. DFN types are `!Send` (see above).
4. Cargo 1.100 warns on unused workspace deps and unused crate deps → planned pins live in a
   commented block in the workspace manifest; move them in when you use them.
