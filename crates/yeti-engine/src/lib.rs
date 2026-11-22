//! Yeti Studio audio engine and the **UI-facing handle** [`Engine`].
//!
//! Owner: **Device & engine engineer**.
//!
//! The UI (`yeti-app`) only talks to `Arc<dyn Engine>`: [`YetiEngine`] in the app,
//! [`MockEngine`] for UI development and tests. Pipeline (PLAN.md §3):
//! `Yeti → capture (cpal) → SPSC ring → DSP thread → SPSC ring → output (cpal) → virtual mic`,
//! plus meter/waveform taps, monitor ("hear myself") and loopback AEC reference.
//!
//! Threading contract for every implementation:
//! - All methods take `&self`, are thread-safe, **non-blocking** (no waiting on audio threads)
//!   and cheap enough to call every UI frame (meters/waveform/stats/poll_events).
//! - Parameter setters publish via atomics/triple-buffer; they never touch cpal callbacks directly.
//! - Device commands may do OS IPC (a few ms) — call them on user action, not per frame.

use yeti_types::{
    AbState, AbVariant, DeviceCommand, DeviceState, DspParams, EngineEvent, EngineStats,
    EngineStatus, MeterSnapshot, MonitorSettings, Preset, Result, WaveformSnapshot,
};

pub mod ab;
pub mod drift;
#[cfg(target_os = "macos")]
pub mod mac_feed;
#[cfg(target_os = "macos")]
pub mod mac_tap;
mod mock;
pub mod pipeline;
pub mod probe;
mod real;
pub mod resample;
mod rt_priority;
pub mod telemetry;
/// Windows routing: VB-CABLE output, Yeti endpoints, WASAPI loopback (Windows engineer).
#[cfg(windows)]
pub mod windows_routing;

pub use mock::MockEngine;
pub use real::{
    DEFAULT_BUFFER_FRAMES, EngineDiagnostics, MAC_FEED_DEVICE, WIN_FEED_DEVICE, YetiEngine,
};

/// Where audio goes. `None` fields = automatic choice.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EngineConfig {
    /// Capture device name (substring match); `None` = first device matching the Yeti.
    pub input_device: Option<String>,
    /// Virtual mic feed device (name, substring match); `None` = platform default
    /// (mac: "Yeti Studio Mic (Feed)", opened by UID through a private aggregate because the
    /// driver hides it; win: "CABLE Input (VB-Audio Virtual Cable)"). If it is not installed a
    /// `Warning` event is queued and no main output is used (mac: install hint; win: VB-CABLE
    /// install message). Playing on speakers is opt-in only (`Some("default")`).
    /// Special values: `"default"` = system default output, `"none"` = no main output.
    pub output_device: Option<String>,
    /// Monitor ("hear myself") device; `None` = Yeti headphone output (`"none"` / `"default"`
    /// as for `output_device`). The stream runs while the engine runs; audio is only sent
    /// while `MonitorSettings::enabled`.
    pub monitor_device: Option<String>,
    /// Callback buffer size in frames; `None` = [`DEFAULT_BUFFER_FRAMES`] (macOS 128, else
    /// the device default).
    pub buffer_frames: Option<u32>,
    /// Loopback reference capture (AEC far end): `None` = automatic (echo cancel enabled
    /// and `aec` compiled in), `Some(b)` = force on/off (diagnostics).
    pub capture_reference: Option<bool>,
}

/// Handle the UI uses to drive the audio engine. Object-safe: use `Arc<dyn Engine>`.
pub trait Engine: Send + Sync {
    // --- lifecycle -------------------------------------------------------
    /// Open devices and start streaming. Idempotent while running.
    /// If the Yeti is absent, status becomes `WaitingForDevice` and the engine
    /// starts automatically on reconnect.
    fn start(&self, config: EngineConfig) -> Result<()>;
    /// Stop all streams and the DSP thread. Idempotent.
    fn stop(&self);
    fn status(&self) -> EngineStatus;

    // --- voice chain -----------------------------------------------------
    /// Replace all DSP parameters (marks preset as `Custom` unless equal to a preset).
    fn set_params(&self, params: DspParams);
    fn params(&self) -> DspParams;
    /// Load a built-in preset's parameters (`DspParams::for_preset`).
    fn set_preset(&self, preset: Preset);
    fn preset(&self) -> Preset;
    /// Software mute of the virtual mic output (hotkey target). Shorthand for
    /// toggling `DspParams::output_muted`.
    fn set_muted(&self, muted: bool);
    fn is_muted(&self) -> bool;

    // --- monitoring ------------------------------------------------------
    /// "Hear myself": processed signal to the monitor device.
    fn set_monitor(&self, monitor: MonitorSettings);
    fn monitor(&self) -> MonitorSettings;

    // --- telemetry (poll every UI frame) --------------------------------
    fn meters(&self) -> MeterSnapshot;
    /// Fill `out` with the latest raw-vs-cleaned waveform (reuses its allocation).
    fn waveform(&self, out: &mut WaveformSnapshot);
    /// Latency, CPU load, drift ratio and under/overrun counters.
    fn stats(&self) -> EngineStats;
    /// Drain pending events (status changes, device hot-plug, warnings). Never blocks.
    fn poll_events(&self) -> Vec<EngineEvent>;

    // --- device ----------------------------------------------------------
    /// Last known Yeti state (`None` if never seen).
    fn device_state(&self) -> Option<DeviceState>;
    /// Change a Yeti hardware setting (gain, mute, headphone, rate).
    fn device_command(&self, cmd: DeviceCommand) -> Result<()>;

    // --- A/B test --------------------------------------------------------
    /// Record `seconds` of raw + cleaned audio simultaneously.
    fn ab_record(&self, seconds: f32) -> Result<()>;
    /// Play a recorded take through the monitor device.
    fn ab_play(&self, variant: AbVariant) -> Result<()>;
    fn ab_stop(&self);
    fn ab_state(&self) -> AbState;

    /// Whether echo cancel is available in this build (feature `aec`); UI greys the card out otherwise.
    fn aec_available(&self) -> bool;
}
