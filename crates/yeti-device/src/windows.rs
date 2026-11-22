//! Windows backend: `IMMDeviceEnumerator` + `IAudioEndpointVolume` (`windows` crate).
//!
//! Phase 6 TODO (Windows engineer):
//! - find capture endpoint whose friendly name contains [`crate::YETI_NAME`]
//! - gain/mute via `IAudioEndpointVolume`; headphone via the render endpoint
//! - sample rate is **read-only** (no public API) → `set_sample_rate` returns `Unsupported`
//! - hot-plug via `IMMNotificationClient`
//!
//! Note: this module shadows the `windows` crate name; refer to the crate as `::windows`.

use crate::{DeviceControl, DeviceEventCallback};
use yeti_types::{DeviceState, Result, YetiError};

/// COM interfaces the backend is built on (also proves the bindings cross-compile).
pub type DeviceEnumerator = ::windows::Win32::Media::Audio::IMMDeviceEnumerator;
pub type EndpointVolume = ::windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;

/// WASAPI/MMDevice-backed Yeti. Stub until Phase 6.
pub struct WasapiYeti {
    _private: (),
}

impl WasapiYeti {
    pub fn open() -> Result<Self> {
        Err(YetiError::NotImplemented("Windows endpoint control"))
    }
}

impl DeviceControl for WasapiYeti {
    fn state(&self) -> Result<DeviceState> {
        Err(YetiError::NotImplemented("WasapiYeti::state"))
    }
    fn set_input_gain_db(&self, _db: f32) -> Result<()> {
        Err(YetiError::NotImplemented("WasapiYeti::set_input_gain_db"))
    }
    fn set_input_muted(&self, _muted: bool) -> Result<()> {
        Err(YetiError::NotImplemented("WasapiYeti::set_input_muted"))
    }
    fn set_headphone_volume(&self, _volume: f32) -> Result<()> {
        Err(YetiError::NotImplemented(
            "WasapiYeti::set_headphone_volume",
        ))
    }
    fn set_headphone_muted(&self, _muted: bool) -> Result<()> {
        Err(YetiError::NotImplemented("WasapiYeti::set_headphone_muted"))
    }
    fn set_sample_rate(&self, _hz: u32) -> Result<()> {
        Err(YetiError::Unsupported(
            "sample rate is read-only on Windows",
        ))
    }
    fn set_event_callback(&self, _cb: Option<DeviceEventCallback>) {}
}
