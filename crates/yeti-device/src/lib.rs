//! Blue Yeti discovery and hardware control.
//!
//! Owner: **Device & engine engineer** (Windows backend: **Windows engineer**).
//!
//! Contract: [`DeviceControl`]. Obtain the platform implementation with [`open_yeti`];
//! use [`MockDevice`] in tests and in the UI before hardware work lands.
//!
//! Identification: USB VID:PID `046d:0ab7`, OS name contains [`YETI_NAME`].
//! Not software-controllable (hardware knobs): polar pattern, analog gain.

use std::sync::Arc;
use yeti_types::{DeviceCommand, DeviceEvent, DeviceState, Result};

#[cfg(target_os = "macos")]
pub mod macos;
mod mock;
#[cfg(windows)]
pub mod windows;

pub use mock::MockDevice;

/// Substring used to recognise the Yeti in OS device names.
pub const YETI_NAME: &str = "Yeti Stereo Microphone";
/// USB vendor id (Logitech, post-acquisition Blue Yeti).
pub const YETI_USB_VID: u16 = 0x046d;
/// USB product id.
pub const YETI_USB_PID: u16 = 0x0ab7;

/// Callback invoked on hot-plug and external property changes.
///
/// May be called from an OS notification thread: implementations must be quick,
/// non-blocking and must not call back into the [`DeviceControl`] synchronously.
pub type DeviceEventCallback = Arc<dyn Fn(DeviceEvent) + Send + Sync + 'static>;

/// Platform-neutral control surface for the Blue Yeti.
///
/// All setters validate ranges and return [`yeti_types::YetiError::OutOfRange`] /
/// [`yeti_types::YetiError::Unsupported`] rather than clamping silently.
/// Implementations are `Send + Sync`; calls are **not** real-time safe (they
/// do OS IPC) and must never be made from an audio callback.
pub trait DeviceControl: Send + Sync {
    /// Current state, freshly queried from the OS.
    fn state(&self) -> Result<DeviceState>;

    /// Set input (capture) volume in dB, within `state().input_gain_range_db`.
    fn set_input_gain_db(&self, db: f32) -> Result<()>;
    /// Set OS/hardware input mute.
    fn set_input_muted(&self, muted: bool) -> Result<()>;
    /// Set headphone (output side) volume, scalar 0..=1.
    fn set_headphone_volume(&self, volume: f32) -> Result<()>;
    /// Set headphone mute.
    fn set_headphone_muted(&self, muted: bool) -> Result<()>;
    /// Set nominal sample rate. Windows returns `Unsupported` (read-only).
    fn set_sample_rate(&self, hz: u32) -> Result<()>;

    /// Register the hot-plug / change callback (replaces any previous one).
    fn set_event_callback(&self, cb: Option<DeviceEventCallback>);

    /// Dispatch a [`DeviceCommand`] to the matching setter.
    fn apply(&self, cmd: DeviceCommand) -> Result<()> {
        match cmd {
            DeviceCommand::SetInputGainDb(db) => self.set_input_gain_db(db),
            DeviceCommand::SetInputMuted(m) => self.set_input_muted(m),
            DeviceCommand::SetHeadphoneVolume(v) => self.set_headphone_volume(v),
            DeviceCommand::SetHeadphoneMuted(m) => self.set_headphone_muted(m),
            DeviceCommand::SetSampleRate(hz) => self.set_sample_rate(hz),
        }
    }
}

/// Open the platform implementation for the connected Yeti.
///
/// Returns [`yeti_types::YetiError::DeviceNotFound`] if no Yeti is present, or
/// `NotImplemented` until the platform backend is written.
pub fn open_yeti() -> Result<Box<dyn DeviceControl>> {
    #[cfg(target_os = "macos")]
    {
        macos::CoreAudioYeti::open().map(|d| Box::new(d) as Box<dyn DeviceControl>)
    }
    #[cfg(windows)]
    {
        self::windows::WasapiYeti::open().map(|d| Box::new(d) as Box<dyn DeviceControl>)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err(yeti_types::YetiError::Unsupported("platform"))
    }
}
