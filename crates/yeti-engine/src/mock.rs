//! [`MockEngine`]: a fake engine with plausible, time-varying telemetry so the UI can be
//! built and tested (egui_kittest) without audio hardware. Deterministic given a clock.

use crate::{Engine, EngineConfig};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use yeti_device::{DeviceControl, MockDevice};
use yeti_types::{
    AbState, AbVariant, DeviceCommand, DeviceEvent, DeviceState, DspParams, EngineCounters,
    EngineEvent, EngineStats, EngineStatus, METER_FLOOR_DB, MeterSnapshot, MonitorSettings, Preset,
    Result, WaveformSnapshot, YetiError,
};

/// Number of waveform buckets the mock produces (~4 s at 20 ms/bucket).
pub const MOCK_WAVEFORM_BUCKETS: usize = 200;
const BUCKET_MS: f32 = 20.0;

struct State {
    status: EngineStatus,
    params: DspParams,
    preset: Preset,
    monitor: MonitorSettings,
    started: Option<Instant>,
    events: VecDeque<EngineEvent>,
    ab: AbPhase,
}

enum AbPhase {
    Idle,
    Recording {
        since: Instant,
        total: f32,
    },
    Ready {
        len: f32,
    },
    Playing {
        since: Instant,
        len: f32,
        variant: AbVariant,
    },
}

/// Fake [`Engine`]. Telemetry is synthesised from wall-clock time:
/// a ~0.5 Hz "speech" envelope over a constant noise floor; cleaned = raw minus noise.
pub struct MockEngine {
    state: Mutex<State>,
    device: MockDevice,
    epoch: Instant,
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MockEngine {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                status: EngineStatus::Stopped,
                params: DspParams::for_preset(Preset::Gaming),
                preset: Preset::Gaming,
                monitor: MonitorSettings::default(),
                started: None,
                events: VecDeque::new(),
                ab: AbPhase::Idle,
            }),
            device: MockDevice::new(),
            epoch: Instant::now(),
        }
    }

    /// Access the fake device (e.g. to simulate unplug in tests).
    pub fn mock_device(&self) -> &MockDevice {
        &self.device
    }

    /// Simulate the Yeti being unplugged / replugged; queues the matching events.
    pub fn simulate_connected(&self, connected: bool) {
        self.device.set_connected(connected);
        let mut s = self.state.lock().unwrap();
        if connected {
            let st = self.device.state().unwrap_or_default();
            s.events
                .push_back(EngineEvent::Device(DeviceEvent::Connected(st)));
            if s.started.is_some() {
                s.status = EngineStatus::Running;
                s.events
                    .push_back(EngineEvent::Status(EngineStatus::Running));
            }
        } else {
            s.events
                .push_back(EngineEvent::Device(DeviceEvent::Disconnected));
            if s.started.is_some() {
                s.status = EngineStatus::WaitingForDevice;
                s.events
                    .push_back(EngineEvent::Status(EngineStatus::WaitingForDevice));
            }
        }
    }

    fn t(&self) -> f32 {
        self.epoch.elapsed().as_secs_f32()
    }

    /// Synthetic raw/cleaned amplitude at time `t` seconds.
    fn sample(t: f32, p: &DspParams) -> (f32, f32) {
        let noise = 0.03 + 0.01 * (t * 13.0).sin().abs();
        let env = ((t * std::f32::consts::PI * 0.5).sin()).max(0.0);
        let speech = 0.5 * env * (0.6 + 0.4 * (t * 37.0).sin().abs());
        let raw = (speech + noise).min(1.0);
        let ns = if p.noise.enabled {
            p.noise.strength.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let cleaned = if p.output_muted {
            0.0
        } else {
            speech + noise * (1.0 - 0.95 * ns)
        };
        (raw, cleaned.min(1.0))
    }

    fn running(s: &State) -> bool {
        matches!(s.status, EngineStatus::Running)
    }

    fn update_ab(s: &mut State) {
        let now = Instant::now();
        match s.ab {
            AbPhase::Recording { since, total }
                if now - since >= Duration::from_secs_f32(total) =>
            {
                s.ab = AbPhase::Ready { len: total };
                s.events.push_back(EngineEvent::AbReady);
            }
            AbPhase::Playing { since, len, .. } if now - since >= Duration::from_secs_f32(len) => {
                s.ab = AbPhase::Ready { len };
            }
            _ => {}
        }
    }
}

fn db(x: f32) -> f32 {
    if x <= 0.0 {
        METER_FLOOR_DB
    } else {
        (20.0 * x.log10()).max(METER_FLOOR_DB)
    }
}

impl Engine for MockEngine {
    fn start(&self, _config: EngineConfig) -> Result<()> {
        let connected = self.device.state().map(|d| d.connected).unwrap_or(false);
        let mut s = self.state.lock().unwrap();
        if s.started.is_none() {
            s.started = Some(Instant::now());
            s.status = if connected {
                EngineStatus::Running
            } else {
                EngineStatus::WaitingForDevice
            };
            let st = s.status.clone();
            s.events.push_back(EngineEvent::Status(st));
        }
        Ok(())
    }

    fn stop(&self) {
        let mut s = self.state.lock().unwrap();
        if s.started.take().is_some() {
            s.status = EngineStatus::Stopped;
            s.events
                .push_back(EngineEvent::Status(EngineStatus::Stopped));
        }
    }

    fn status(&self) -> EngineStatus {
        self.state.lock().unwrap().status.clone()
    }

    fn set_params(&self, params: DspParams) {
        let mut s = self.state.lock().unwrap();
        s.preset = [Preset::Gaming, Preset::Meeting]
            .into_iter()
            .find(|p| DspParams::for_preset(*p) == params)
            .unwrap_or(Preset::Custom);
        s.params = params;
    }

    fn params(&self) -> DspParams {
        self.state.lock().unwrap().params
    }

    fn set_preset(&self, preset: Preset) {
        let mut s = self.state.lock().unwrap();
        let muted = s.params.output_muted;
        s.params = DspParams::for_preset(preset);
        s.params.output_muted = muted;
        s.preset = preset;
    }

    fn preset(&self) -> Preset {
        self.state.lock().unwrap().preset
    }

    fn set_muted(&self, muted: bool) {
        self.state.lock().unwrap().params.output_muted = muted;
    }

    fn is_muted(&self) -> bool {
        self.state.lock().unwrap().params.output_muted
    }

    fn set_monitor(&self, monitor: MonitorSettings) {
        self.state.lock().unwrap().monitor = monitor;
    }

    fn monitor(&self) -> MonitorSettings {
        self.state.lock().unwrap().monitor
    }

    fn meters(&self) -> MeterSnapshot {
        let s = self.state.lock().unwrap();
        if !Self::running(&s) {
            return MeterSnapshot::default();
        }
        let (raw, clean) = Self::sample(self.t(), &s.params);
        let (ri, ro) = (db(raw * 0.7), db(clean * 0.7));
        MeterSnapshot {
            input_peak_db: db(raw),
            input_rms_db: ri,
            output_peak_db: db(clean),
            output_rms_db: ro,
            noise_reduction_db: (ri - ro).max(0.0),
            gain_change_db: if s.params.auto_level.enabled {
                -2.0 * s.params.auto_level.firmness
            } else {
                0.0
            },
            gate_open: clean > 0.06,
            voice_probability: ((raw - 0.04) * 3.0).clamp(0.0, 1.0),
        }
    }

    fn waveform(&self, out: &mut WaveformSnapshot) {
        let s = self.state.lock().unwrap();
        out.raw.clear();
        out.cleaned.clear();
        out.bucket_ms = BUCKET_MS;
        if !Self::running(&s) {
            return;
        }
        let now = self.t();
        for i in 0..MOCK_WAVEFORM_BUCKETS {
            let t = now - (MOCK_WAVEFORM_BUCKETS - 1 - i) as f32 * BUCKET_MS / 1000.0;
            let (r, c) = Self::sample(t, &s.params);
            out.raw.push(r);
            out.cleaned.push(c);
        }
    }

    fn stats(&self) -> EngineStats {
        let s = self.state.lock().unwrap();
        let Some(started) = s.started else {
            return EngineStats::default();
        };
        let meeting = s.params.echo_cancel.enabled;
        let blocks = (started.elapsed().as_millis() / 10) as u64;
        EngineStats {
            latency_ms: if meeting { 34.0 } else { 22.0 },
            dsp_latency_ms: if meeting { 20.0 } else { 10.0 },
            cpu_percent: if meeting { 7.5 } else { 4.2 },
            drift_ratio: 1.0,
            counters: EngineCounters {
                blocks_processed: blocks,
                ..Default::default()
            },
        }
    }

    fn poll_events(&self) -> Vec<EngineEvent> {
        let mut s = self.state.lock().unwrap();
        Self::update_ab(&mut s);
        s.events.drain(..).collect()
    }

    fn device_state(&self) -> Option<DeviceState> {
        self.device.state().ok()
    }

    fn device_command(&self, cmd: DeviceCommand) -> Result<()> {
        self.device.apply(cmd)?;
        if let Ok(st) = self.device.state() {
            self.state
                .lock()
                .unwrap()
                .events
                .push_back(EngineEvent::Device(DeviceEvent::Changed(st)));
        }
        Ok(())
    }

    fn ab_record(&self, seconds: f32) -> Result<()> {
        if !(0.5..=60.0).contains(&seconds) {
            return Err(YetiError::OutOfRange(format!("A/B length {seconds} s")));
        }
        let mut s = self.state.lock().unwrap();
        if !Self::running(&s) {
            return Err(YetiError::NotRunning);
        }
        s.ab = AbPhase::Recording {
            since: Instant::now(),
            total: seconds,
        };
        Ok(())
    }

    fn ab_play(&self, variant: AbVariant) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        Self::update_ab(&mut s);
        let len = match s.ab {
            AbPhase::Ready { len } | AbPhase::Playing { len, .. } => len,
            _ => return Err(YetiError::Backend("no A/B take recorded".into())),
        };
        s.ab = AbPhase::Playing {
            since: Instant::now(),
            len,
            variant,
        };
        Ok(())
    }

    fn ab_stop(&self) {
        let mut s = self.state.lock().unwrap();
        match s.ab {
            AbPhase::Playing { len, .. } => s.ab = AbPhase::Ready { len },
            AbPhase::Recording { .. } => s.ab = AbPhase::Idle,
            _ => {}
        }
    }

    fn ab_state(&self) -> AbState {
        let mut s = self.state.lock().unwrap();
        Self::update_ab(&mut s);
        match s.ab {
            AbPhase::Idle => AbState::Idle,
            AbPhase::Recording { since, total } => AbState::Recording {
                elapsed_s: since.elapsed().as_secs_f32(),
                total_s: total,
            },
            AbPhase::Ready { len } => AbState::Ready { length_s: len },
            AbPhase::Playing { since, variant, .. } => AbState::Playing {
                variant,
                position_s: since.elapsed().as_secs_f32(),
            },
        }
    }

    fn aec_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mock_engine_lifecycle_and_telemetry() {
        let e: Arc<dyn Engine> = Arc::new(MockEngine::new());
        assert_eq!(e.status(), EngineStatus::Stopped);
        e.start(EngineConfig::default()).unwrap();
        assert_eq!(e.status(), EngineStatus::Running);
        assert!(matches!(
            e.poll_events().as_slice(),
            [EngineEvent::Status(EngineStatus::Running)]
        ));
        let mut w = WaveformSnapshot::default();
        e.waveform(&mut w);
        assert_eq!(w.raw.len(), MOCK_WAVEFORM_BUCKETS);
        assert_eq!(w.raw.len(), w.cleaned.len());
        e.set_preset(Preset::Meeting);
        assert!(e.params().echo_cancel.enabled);
        let mut p = e.params();
        p.noise.strength = 0.1;
        e.set_params(p);
        assert_eq!(e.preset(), Preset::Custom);
        e.set_muted(true);
        assert!(e.is_muted());
        e.device_command(DeviceCommand::SetInputGainDb(1.0))
            .unwrap();
        assert_eq!(e.device_state().unwrap().input_gain_db, Some(1.0));
        e.stop();
        assert_eq!(e.status(), EngineStatus::Stopped);
    }

    #[test]
    fn mock_engine_hotplug() {
        let m = MockEngine::new();
        m.start(EngineConfig::default()).unwrap();
        m.poll_events();
        m.simulate_connected(false);
        assert_eq!(m.status(), EngineStatus::WaitingForDevice);
        m.simulate_connected(true);
        assert_eq!(m.status(), EngineStatus::Running);
    }
}
