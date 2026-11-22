//! macOS backend: CoreAudio HAL properties via `objc2-core-audio`.
//!
//! Phase 1 TODO (Device & engine engineer):
//! - enumerate `kAudioHardwarePropertyDevices`, match name/UID → input + output device ids
//! - input volume: `kAudioDevicePropertyVolumeDecibels` scope input, elements 1,2
//! - mute: `kAudioDevicePropertyMute` element 0; nominal rate: `kAudioDevicePropertyNominalSampleRate`
//! - listeners for `kAudioHardwarePropertyDevices` + per-property changes → [`DeviceEvent`](yeti_types::DeviceEvent)

use crate::{DeviceControl, DeviceEventCallback};
use yeti_types::{DeviceState, Result, YetiError};

/// The HAL system object (root of device enumeration). Also proves the CoreAudio
/// bindings link in the scaffold.
pub const SYSTEM_OBJECT: u32 = objc2_core_audio::kAudioObjectSystemObject as u32;

/// CoreAudio-backed Yeti. Stub until Phase 1.
pub struct CoreAudioYeti {
    _private: (),
}

impl CoreAudioYeti {
    /// Locate the Yeti's CoreAudio input/output devices.
    pub fn open() -> Result<Self> {
        Err(YetiError::NotImplemented("macOS CoreAudio device control"))
    }
}

impl DeviceControl for CoreAudioYeti {
    fn state(&self) -> Result<DeviceState> {
        Err(YetiError::NotImplemented("CoreAudioYeti::state"))
    }
    fn set_input_gain_db(&self, _db: f32) -> Result<()> {
        Err(YetiError::NotImplemented(
            "CoreAudioYeti::set_input_gain_db",
        ))
    }
    fn set_input_muted(&self, _muted: bool) -> Result<()> {
        Err(YetiError::NotImplemented("CoreAudioYeti::set_input_muted"))
    }
    fn set_headphone_volume(&self, _volume: f32) -> Result<()> {
        Err(YetiError::NotImplemented(
            "CoreAudioYeti::set_headphone_volume",
        ))
    }
    fn set_headphone_muted(&self, _muted: bool) -> Result<()> {
        Err(YetiError::NotImplemented(
            "CoreAudioYeti::set_headphone_muted",
        ))
    }
    fn set_sample_rate(&self, _hz: u32) -> Result<()> {
        Err(YetiError::NotImplemented("CoreAudioYeti::set_sample_rate"))
    }
    fn set_event_callback(&self, _cb: Option<DeviceEventCallback>) {}
}
