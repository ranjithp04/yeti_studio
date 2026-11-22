//! Shared, dependency-light types used across every Yeti Studio crate.
//!
//! Owner: **Architect**. Changes here affect all crates: keep them *additive*
//! (new fields need `#[serde(default)]`, new enum variants are fine) and note them
//! in `docs/API.md`.
//!
//! Conventions:
//! - Audio inside the engine is **48 kHz, mono, f32, 480-frame (10 ms) blocks**.
//! - Levels are in dBFS (`f32`, `-inf`..=0) unless the name says otherwise.
//! - All parameter structs are `Serialize + Deserialize` so they can live in the TOML config.

use serde::{Deserialize, Serialize};

/// Internal processing sample rate (Hz).
pub const ENGINE_SAMPLE_RATE: u32 = 48_000;
/// Fixed DSP block size in frames (10 ms @ 48 kHz).
pub const BLOCK_FRAMES: usize = 480;
/// Lowest level reported by meters; use instead of `-inf` for display.
pub const METER_FLOOR_DB: f32 = -90.0;

mod params;
pub use params::*;

// ---------------------------------------------------------------------------
// UI-level enums
// ---------------------------------------------------------------------------

/// Colour scheme selection (Light / Dark / System switch in the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the OS appearance.
    #[default]
    System,
}

/// Built-in voice presets. `Custom` = user moved a control away from a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// Lowest latency; no echo cancel.
    #[default]
    Gaming,
    /// Echo cancel on, firm auto-level.
    Meeting,
    /// Parameters edited by the user after picking a preset.
    Custom,
}

/// Which window surface is shown (PLAN.md §2: A Simple, B Pro, C Mini).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    #[default]
    Simple,
    Pro,
    Mini,
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

/// Snapshot of the microphone's software-controllable state.
///
/// Fields that a platform cannot read are `None` (e.g. headphone volume when
/// the output side is missing).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceState {
    /// `true` while the Yeti is enumerated with its audio interfaces.
    pub connected: bool,
    /// OS display name, e.g. "Yeti Stereo Microphone".
    pub name: String,
    /// Platform-specific stable id (CoreAudio UID / Windows endpoint id).
    pub uid: String,
    /// Input (capture) volume in dB, as reported by the OS.
    pub input_gain_db: Option<f32>,
    /// Settable input gain range in dB `(min, max)`.
    pub input_gain_range_db: Option<(f32, f32)>,
    /// Hardware/OS input mute.
    pub input_muted: Option<bool>,
    /// Headphone (output side) volume, scalar 0..=1.
    pub headphone_volume: Option<f32>,
    pub headphone_muted: Option<bool>,
    /// Current nominal sample rate (Hz).
    pub sample_rate: Option<u32>,
    /// Rates the device accepts (empty = unknown).
    pub available_sample_rates: Vec<u32>,
    /// `false` on Windows (no public API to change device rate) — UI shows read-only.
    pub sample_rate_settable: bool,
}

/// Commands the UI can send to the microphone (via the engine handle).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DeviceCommand {
    SetInputGainDb(f32),
    SetInputMuted(bool),
    SetHeadphoneVolume(f32),
    SetHeadphoneMuted(bool),
    SetSampleRate(u32),
}

/// Hot-plug / property-change notifications from the device layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceEvent {
    Connected(DeviceState),
    Disconnected,
    /// A property changed outside the app (e.g. the Yeti mute button, System Settings).
    Changed(DeviceState),
}

// ---------------------------------------------------------------------------
// Meters / waveform / stats
// ---------------------------------------------------------------------------

/// Instantaneous meter values; cheap `Copy` so the UI can poll every frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeterSnapshot {
    pub input_peak_db: f32,
    pub input_rms_db: f32,
    pub output_peak_db: f32,
    pub output_rms_db: f32,
    /// Noise removed (input RMS − output RMS, ≥ 0), dB.
    pub noise_reduction_db: f32,
    /// Auto-level / limiter gain change (negative = reduction), dB.
    pub gain_change_db: f32,
    /// Voice gate currently open.
    pub gate_open: bool,
    /// Voice-activity probability 0..=1 (from the NS model or gate detector).
    pub voice_probability: f32,
}

impl Default for MeterSnapshot {
    fn default() -> Self {
        Self {
            input_peak_db: METER_FLOOR_DB,
            input_rms_db: METER_FLOOR_DB,
            output_peak_db: METER_FLOOR_DB,
            output_rms_db: METER_FLOOR_DB,
            noise_reduction_db: 0.0,
            gain_change_db: 0.0,
            gate_open: false,
            voice_probability: 0.0,
        }
    }
}

/// Rolling raw-vs-cleaned waveform for the UI.
///
/// Each element is the peak absolute amplitude (0..=1) of one display bucket,
/// oldest first. `raw` and `cleaned` always have equal length.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WaveformSnapshot {
    pub raw: Vec<f32>,
    pub cleaned: Vec<f32>,
    /// Duration covered by one bucket, in milliseconds.
    pub bucket_ms: f32,
}

/// Monotonic error counters since the engine was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EngineCounters {
    /// cpal-reported `Xrun`/stream errors.
    pub stream_xruns: u64,
    /// Capture→DSP ring was full (samples dropped).
    pub capture_overruns: u64,
    /// DSP→output ring was empty (silence emitted).
    pub output_underruns: u64,
    /// Far-end (loopback) reference underruns, Meeting preset only.
    pub reference_underruns: u64,
    /// DSP blocks processed.
    pub blocks_processed: u64,
}

/// Latency / CPU / counters for the status bar and Pro view.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct EngineStats {
    /// Estimated end-to-end latency (capture → virtual mic) in ms.
    pub latency_ms: f32,
    /// Algorithmic latency of the DSP chain alone in ms.
    pub dsp_latency_ms: f32,
    /// DSP thread load: processing time / real time, percent (0..100+).
    pub cpu_percent: f32,
    /// Capture→output drift-bridge resample ratio (1.0 = no drift).
    pub drift_ratio: f64,
    pub counters: EngineCounters,
}

/// Engine lifecycle state.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum EngineStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    /// Waiting for the Yeti to be (re)connected.
    WaitingForDevice,
    Error(String),
}

/// Asynchronous notifications from the engine to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EngineEvent {
    Status(EngineStatus),
    Device(DeviceEvent),
    /// A/B recording finished and is ready for playback.
    AbReady,
    /// Non-fatal warning to surface in the UI (e.g. "virtual mic not installed").
    Warning(String),
}

/// A/B test: which take to play back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AbVariant {
    /// Unprocessed Yeti signal.
    Raw,
    /// Through the voice chain.
    Cleaned,
}

/// Progress of the A/B test recorder.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum AbState {
    #[default]
    Idle,
    Recording {
        elapsed_s: f32,
        total_s: f32,
    },
    Ready {
        length_s: f32,
    },
    Playing {
        variant: AbVariant,
        position_s: f32,
    },
}

/// "Hear myself" monitor settings (processed signal to the Yeti headphone jack).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonitorSettings {
    pub enabled: bool,
    /// Linear gain 0..=1.
    pub volume: f32,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            volume: 0.8,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error type shared across crates.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum YetiError {
    #[error("Blue Yeti not found")]
    DeviceNotFound,
    #[error("device disconnected")]
    DeviceDisconnected,
    #[error("property not supported on this platform/device: {0}")]
    Unsupported(&'static str),
    #[error("value out of range: {0}")]
    OutOfRange(String),
    #[error("virtual microphone not installed: {0}")]
    VirtualMicMissing(String),
    #[error("audio backend error: {0}")]
    Backend(String),
    #[error("engine is not running")]
    NotRunning,
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias.
pub type Result<T, E = YetiError> = std::result::Result<T, E>;
