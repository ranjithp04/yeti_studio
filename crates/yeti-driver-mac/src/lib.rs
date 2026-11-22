//! macOS virtual microphone: AudioServerPlugIn bundle `YetiStudioMic.driver`.
//!
//! Owner: **macOS driver engineer** (Phase 5). Excluded from Windows checks; compiles to an
//! empty library (constants only) on non-macOS targets.
//!
//! Contract (PLAN.md §3, Phase 5):
//! - 2-ch 48 kHz f32 loopback. Input device "Yeti Studio Mic" (visible, can be the default
//!   input); output device "Yeti Studio Mic (Feed)" with `IsHidden = 1`,
//!   `CanBeDefaultDevice = 0` and `CanBeDefaultSystemDevice = 0`.
//! - Exports the CFPlugIn factory [`FACTORY_FUNCTION`] named in Info.plist (`CFPlugInFactories`).
//! - Every FFI entry point runs inside `catch_unwind`; no allocation or locks in `DoIOOperation`.
//!
//! **Finding the Feed device:** hidden devices are NOT listed in `kAudioHardwarePropertyDevices`
//! (so cpal's device enumeration will not show it). Resolve it with
//! `kAudioHardwarePropertyTranslateUIDToDevice` and [`FEED_DEVICE_UID`].

/// Device names shared with the engine (which routes its output to the Feed device).
pub const MIC_DEVICE_NAME: &str = "Yeti Studio Mic";
pub const FEED_DEVICE_NAME: &str = "Yeti Studio Mic (Feed)";
/// `kAudioDevicePropertyDeviceUID` of the visible input device.
pub const MIC_DEVICE_UID: &str = "YetiStudioMic_UID";
/// `kAudioDevicePropertyDeviceUID` of the hidden Feed (output) device.
pub const FEED_DEVICE_UID: &str = "YetiStudioMicFeed_UID";
/// `kAudioDevicePropertyModelUID` (both devices).
pub const MODEL_UID: &str = "YetiStudioMic_ModelUID";
pub const MANUFACTURER: &str = "Yeti Studio";
/// Bundle identifier used in Info.plist.
pub const BUNDLE_ID: &str = "local.yetistudio.driver";
/// Executable name inside `YetiStudioMic.driver/Contents/MacOS/`.
pub const BUNDLE_EXECUTABLE: &str = "YetiStudioMic";
/// CFPlugIn factory UUID (Info.plist `CFPlugInFactories` / `CFPlugInTypes`).
pub const FACTORY_UUID: &str = "3E68D160-EB85-4D65-BD4B-BF24E8EC72B4";
/// kAudioServerPlugInTypeUUID.
pub const PLUGIN_TYPE_UUID: &str = "443ABAB8-E7B3-491A-B985-BEB9187030DB";
/// Exported factory symbol.
pub const FACTORY_FUNCTION: &str = "YetiStudioMic_Create";
/// Fixed stream format.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

/// Info.plist of the bundle (copied verbatim by `cargo xtask bundle-mac`).
pub const INFO_PLIST: &str = include_str!("../Info.plist");

#[cfg(target_os = "macos")]
mod driver;
#[cfg(target_os = "macos")]
pub mod ffi;
#[cfg(target_os = "macos")]
mod loopback;

#[cfg(target_os = "macos")]
pub use driver::{
    FEED_DEVICE_ID, FEED_STREAM_ID, MIC_DEVICE_ID, MIC_STREAM_ID, PLUGIN_ID, YetiStudioMic_Create,
};

// Lets the shared harness (tests/common/harness.rs) say `yeti_driver_mac::...` in unit tests too.
#[cfg(all(test, target_os = "macos"))]
extern crate self as yeti_driver_mac;
#[cfg(all(test, target_os = "macos"))]
mod tests;
