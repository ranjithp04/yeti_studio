//! DSP parameter model. One struct per voice-chain card in the UI
//! (Rumble filter, AI noise removal, Voice gate, Voice EQ + de-esser, Auto-level, Echo cancel)
//! plus the output limiter.

use crate::Preset;
use serde::{Deserialize, Serialize};

/// Noise-suppression model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseModel {
    /// DeepFilterNet 3 (libDF + tract). Higher quality, higher CPU/latency.
    #[default]
    DeepFilterNet,
    /// RNNoise (nnnoiseless). Low CPU fallback.
    RnNoise,
}

/// "Rumble filter" card: high-pass filter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighPassParams {
    pub enabled: bool,
    /// Cut-off frequency in Hz (typ. 60..=150).
    pub cutoff_hz: f32,
}
impl Default for HighPassParams {
    fn default() -> Self {
        Self {
            enabled: true,
            cutoff_hz: 80.0,
        }
    }
}

/// "AI noise removal" card.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseSuppressionParams {
    pub enabled: bool,
    pub model: NoiseModel,
    /// UI "noise strength" slider, 0..=1 (maps to max attenuation in dB).
    pub strength: f32,
}
impl Default for NoiseSuppressionParams {
    fn default() -> Self {
        Self {
            enabled: true,
            model: NoiseModel::default(),
            strength: 0.8,
        }
    }
}

/// "Voice gate" card: VAD-driven downward expander.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateParams {
    pub enabled: bool,
    /// Open threshold in dBFS.
    pub threshold_db: f32,
    /// Attenuation when closed, dB (positive number).
    pub range_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}
impl Default for GateParams {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -50.0,
            range_db: 30.0,
            attack_ms: 2.0,
            release_ms: 150.0,
        }
    }
}

/// "Voice EQ + de-esser" card.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqParams {
    pub enabled: bool,
    /// Low-shelf ("warmth") gain at ~200 Hz, dB.
    pub low_gain_db: f32,
    /// Presence peak at ~3 kHz, dB.
    pub presence_gain_db: f32,
    /// High-shelf ("air") at ~10 kHz, dB.
    pub air_gain_db: f32,
    pub deesser_enabled: bool,
    /// De-esser amount 0..=1.
    pub deesser_amount: f32,
}
impl Default for EqParams {
    fn default() -> Self {
        Self {
            enabled: true,
            low_gain_db: 0.0,
            presence_gain_db: 2.0,
            air_gain_db: 1.0,
            deesser_enabled: true,
            deesser_amount: 0.5,
        }
    }
}

/// "Auto-level" card: AGC / compressor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoLevelParams {
    pub enabled: bool,
    /// Target speech level, dBFS RMS.
    pub target_db: f32,
    /// 0 = gentle, 1 = firm (Meeting).
    pub firmness: f32,
    /// Maximum make-up gain, dB.
    pub max_gain_db: f32,
}
impl Default for AutoLevelParams {
    fn default() -> Self {
        Self {
            enabled: true,
            target_db: -18.0,
            firmness: 0.4,
            max_gain_db: 12.0,
        }
    }
}

/// "Echo cancel" card (Meeting). Only effective when built with the `aec` feature.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EchoCancelParams {
    pub enabled: bool,
}

/// Final safety limiter (not a UI card).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimiterParams {
    pub enabled: bool,
    /// Ceiling in dBFS (≤ −1.0 per Phase 2 acceptance).
    pub ceiling_db: f32,
}
impl Default for LimiterParams {
    fn default() -> Self {
        Self {
            enabled: true,
            ceiling_db: -1.0,
        }
    }
}

/// Full voice-chain configuration. Order of processing:
/// HPF → NS → gate → EQ/de-esser → auto-level → limiter (AEC first when enabled).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DspParams {
    pub high_pass: HighPassParams,
    pub noise: NoiseSuppressionParams,
    pub gate: GateParams,
    pub eq: EqParams,
    pub auto_level: AutoLevelParams,
    pub echo_cancel: EchoCancelParams,
    pub limiter: LimiterParams,
    /// Software output mute (virtual mic receives silence). Independent of Yeti hardware mute.
    pub output_muted: bool,
}

impl DspParams {
    /// Parameters for a built-in preset. `Preset::Custom` returns the defaults.
    pub fn for_preset(preset: Preset) -> Self {
        let mut p = Self::default();
        match preset {
            Preset::Gaming => {
                p.echo_cancel.enabled = false;
                p.auto_level.firmness = 0.3;
            }
            Preset::Meeting => {
                p.echo_cancel.enabled = true;
                p.auto_level.firmness = 0.8;
                p.gate.release_ms = 250.0;
            }
            Preset::Custom => {}
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_differ_in_aec() {
        assert!(!DspParams::for_preset(Preset::Gaming).echo_cancel.enabled);
        assert!(DspParams::for_preset(Preset::Meeting).echo_cancel.enabled);
    }

    #[test]
    fn limiter_ceiling_default_is_minus_one() {
        assert!(DspParams::default().limiter.ceiling_db <= -1.0);
    }
}
