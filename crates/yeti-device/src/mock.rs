//! In-memory [`DeviceControl`] for tests and UI development.

use crate::{DeviceControl, DeviceEventCallback};
use std::sync::Mutex;
use yeti_types::{DeviceEvent, DeviceState, Result, YetiError};

/// Fake Yeti with realistic ranges. Thread-safe; fires `Changed` events on every set.
pub struct MockDevice {
    state: Mutex<DeviceState>,
    cb: Mutex<Option<DeviceEventCallback>>,
}

impl Default for MockDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDevice {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DeviceState {
                connected: true,
                name: crate::YETI_NAME.to_string(),
                uid: "mock-yeti".to_string(),
                input_gain_db: Some(14.7),
                input_gain_range_db: Some((-24.0, 24.0)),
                input_muted: Some(false),
                headphone_volume: Some(0.7),
                headphone_muted: Some(false),
                sample_rate: Some(48_000),
                available_sample_rates: vec![44_100, 48_000],
                sample_rate_settable: true,
            }),
            cb: Mutex::new(None),
        }
    }

    /// Simulate unplug/replug (fires `Disconnected` / `Connected`).
    pub fn set_connected(&self, connected: bool) {
        let snapshot = {
            let mut s = self.state.lock().unwrap();
            s.connected = connected;
            s.clone()
        };
        self.emit(if connected {
            DeviceEvent::Connected(snapshot)
        } else {
            DeviceEvent::Disconnected
        });
    }

    fn emit(&self, ev: DeviceEvent) {
        let cb = self.cb.lock().unwrap().clone();
        if let Some(cb) = cb {
            cb(ev);
        }
    }

    fn update(&self, f: impl FnOnce(&mut DeviceState) -> Result<()>) -> Result<()> {
        let snapshot = {
            let mut s = self.state.lock().unwrap();
            if !s.connected {
                return Err(YetiError::DeviceDisconnected);
            }
            f(&mut s)?;
            s.clone()
        };
        self.emit(DeviceEvent::Changed(snapshot));
        Ok(())
    }
}

impl DeviceControl for MockDevice {
    fn state(&self) -> Result<DeviceState> {
        Ok(self.state.lock().unwrap().clone())
    }

    fn set_input_gain_db(&self, db: f32) -> Result<()> {
        self.update(|s| {
            let (lo, hi) = s.input_gain_range_db.unwrap_or((0.0, 0.0));
            if !(lo..=hi).contains(&db) {
                return Err(YetiError::OutOfRange(format!("{db} dB not in {lo}..={hi}")));
            }
            s.input_gain_db = Some(db);
            Ok(())
        })
    }

    fn set_input_muted(&self, muted: bool) -> Result<()> {
        self.update(|s| {
            s.input_muted = Some(muted);
            Ok(())
        })
    }

    fn set_headphone_volume(&self, volume: f32) -> Result<()> {
        self.update(|s| {
            if !(0.0..=1.0).contains(&volume) {
                return Err(YetiError::OutOfRange(format!("volume {volume}")));
            }
            s.headphone_volume = Some(volume);
            Ok(())
        })
    }

    fn set_headphone_muted(&self, muted: bool) -> Result<()> {
        self.update(|s| {
            s.headphone_muted = Some(muted);
            Ok(())
        })
    }

    fn set_sample_rate(&self, hz: u32) -> Result<()> {
        self.update(|s| {
            if !s.available_sample_rates.contains(&hz) {
                return Err(YetiError::OutOfRange(format!("{hz} Hz")));
            }
            s.sample_rate = Some(hz);
            Ok(())
        })
    }

    fn set_event_callback(&self, cb: Option<DeviceEventCallback>) {
        *self.cb.lock().unwrap() = cb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yeti_types::DeviceCommand;

    #[test]
    fn set_and_restore_gain_fires_events() {
        let d = MockDevice::new();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        d.set_event_callback(Some(Arc::new(move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
        })));
        let orig = d.state().unwrap().input_gain_db.unwrap();
        d.apply(DeviceCommand::SetInputGainDb(3.0)).unwrap();
        assert_eq!(d.state().unwrap().input_gain_db, Some(3.0));
        d.apply(DeviceCommand::SetInputGainDb(orig)).unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert!(d.set_input_gain_db(99.0).is_err());
    }
}
