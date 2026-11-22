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
- `MockDevice` = working in-memory implementation (+ `set_connected` to simulate hot-plug).
- Setters validate and return `OutOfRange`; never called from audio callbacks.
- Identification constants: `YETI_NAME`, `YETI_USB_VID` (0x046d), `YETI_USB_PID` (0x0ab7).

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
- `YetiEngine`: real engine stub (`start` → `NotImplemented`).

## `yeti-driver-mac`

cdylib + rlib. Exports `YetiStudioMic_Create(CFAllocatorRef, CFUUIDRef) -> *mut c_void` (stub returns NULL).
Constants `MIC_DEVICE_NAME` = "Yeti Studio Mic", `FEED_DEVICE_NAME` = "Yeti Studio Mic (Feed)", `BUNDLE_ID`.
Empty on non-macOS; excluded from Windows checks.

## Features & build matrix

| Feature | Crates | Default | Windows check |
|---|---|---|---|
| `dfn` | yeti-dsp → yeti-engine → yeti-app | on | **off** (tract-linalg needs `llvm-lib`, absent on this Mac) |
| `aec` | same chain | off | off |

## xtask

- `cargo xtask check-windows [--with-dfn]` — `cargo xwin check --target x86_64-pc-windows-msvc --no-default-features` on all crates except `yeti-driver-mac`.
- `cargo xtask bundle-mac` — release build; bundle assembly TODO (Phase 5).
- `cargo xtask install-driver` — prints sudo install/uninstall commands only.

## Gotchas found in Phase 0

1. libDF does not compile against tract 0.21.19 → tract pinned `=0.21.4` (workspace).
2. tract 0.21.4 debug-assertions reject the DFN3 model ("duplicate name /convt3/Conv.bias") →
   `debug-assertions = false` for tract-*/deep_filter in `[profile.dev.package]`.
3. DFN types are `!Send` (see above).
4. Cargo 1.100 warns on unused workspace deps and unused crate deps → planned pins live in a
   commented block in the workspace manifest; move them in when you use them.
