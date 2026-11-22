//! The real engine. Stub until Phase 1 (Device & engine engineer).

use crate::{Engine, EngineConfig};
use std::sync::Mutex;
use yeti_types::{
    AbState, AbVariant, DeviceCommand, DeviceState, DspParams, EngineEvent, EngineStats,
    EngineStatus, MeterSnapshot, MonitorSettings, Preset, Result, WaveformSnapshot, YetiError,
};

/// cpal-based engine (capture → DSP thread → virtual mic). **Stub**: `start` returns
/// `NotImplemented`; parameter getters/setters already behave.
pub struct YetiEngine {
    params: Mutex<(DspParams, Preset)>,
    monitor: Mutex<MonitorSettings>,
}

impl Default for YetiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl YetiEngine {
    pub fn new() -> Self {
        Self {
            params: Mutex::new((DspParams::for_preset(Preset::Gaming), Preset::Gaming)),
            monitor: Mutex::new(MonitorSettings::default()),
        }
    }
}

impl Engine for YetiEngine {
    fn start(&self, _config: EngineConfig) -> Result<()> {
        Err(YetiError::NotImplemented("YetiEngine::start"))
    }
    fn stop(&self) {}
    fn status(&self) -> EngineStatus {
        EngineStatus::Stopped
    }
    fn set_params(&self, params: DspParams) {
        *self.params.lock().unwrap() = (params, Preset::Custom);
    }
    fn params(&self) -> DspParams {
        self.params.lock().unwrap().0
    }
    fn set_preset(&self, preset: Preset) {
        *self.params.lock().unwrap() = (DspParams::for_preset(preset), preset);
    }
    fn preset(&self) -> Preset {
        self.params.lock().unwrap().1
    }
    fn set_muted(&self, muted: bool) {
        self.params.lock().unwrap().0.output_muted = muted;
    }
    fn is_muted(&self) -> bool {
        self.params.lock().unwrap().0.output_muted
    }
    fn set_monitor(&self, monitor: MonitorSettings) {
        *self.monitor.lock().unwrap() = monitor;
    }
    fn monitor(&self) -> MonitorSettings {
        *self.monitor.lock().unwrap()
    }
    fn meters(&self) -> MeterSnapshot {
        MeterSnapshot::default()
    }
    fn waveform(&self, out: &mut WaveformSnapshot) {
        out.raw.clear();
        out.cleaned.clear();
    }
    fn stats(&self) -> EngineStats {
        EngineStats::default()
    }
    fn poll_events(&self) -> Vec<EngineEvent> {
        Vec::new()
    }
    fn device_state(&self) -> Option<DeviceState> {
        None
    }
    fn device_command(&self, _cmd: DeviceCommand) -> Result<()> {
        Err(YetiError::NotRunning)
    }
    fn ab_record(&self, _seconds: f32) -> Result<()> {
        Err(YetiError::NotImplemented("YetiEngine::ab_record"))
    }
    fn ab_play(&self, _variant: AbVariant) -> Result<()> {
        Err(YetiError::NotImplemented("YetiEngine::ab_play"))
    }
    fn ab_stop(&self) {}
    fn ab_state(&self) -> AbState {
        AbState::Idle
    }
    fn aec_available(&self) -> bool {
        yeti_dsp::VoiceChain::aec_available()
    }
}
