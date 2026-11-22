//! Open the hidden "Yeti Studio Mic (Feed)" device (yeti-driver-mac) for output.
//!
//! The Feed device is `IsHidden = 1`, so it is not in `kAudioHardwarePropertyDevices` and
//! cpal cannot enumerate it. We resolve it by UID (`kAudioHardwarePropertyTranslateUIDToDevice`)
//! and wrap it in a **private aggregate device** whose only sub-device is the Feed. Private
//! aggregates are listed for the creating process, so cpal can open it by name. The aggregate
//! is destroyed on drop.

use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubDeviceUIDKey,
};
use objc2_core_foundation::{CFDictionary, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};
use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use yeti_types::{Result, YetiError};

/// UID of the driver's Feed (output) device. Same value as
/// `yeti_driver_mac::FEED_DEVICE_UID` (not a dependency: that crate exports the plug-in
/// factory symbol, which must not end up in the app binary).
pub const FEED_DEVICE_UID: &str = "YetiStudioMicFeed_UID";

static INSTANCE: AtomicU32 = AtomicU32::new(0);

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(k.to_str().unwrap_or_default())
}

/// `AudioObjectID` of the device with this UID, hidden or not; `None` if absent.
pub fn device_for_uid(uid: &str) -> Option<AudioObjectID> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslateUIDToDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let cf_uid = CFString::from_str(uid);
    let cf_ptr: *const CFString = &*cf_uid;
    let mut obj: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: qualifier = CFStringRef (pointer-sized, valid for the call), out = AudioObjectID.
    let st = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&addr),
            size_of::<*const CFString>() as u32,
            (&cf_ptr as *const *const CFString).cast(),
            NonNull::from(&mut size),
            NonNull::from(&mut obj).cast(),
        )
    };
    (st == 0 && obj != 0).then_some(obj)
}

/// A private aggregate device wrapping one (possibly hidden) output device.
pub struct DeviceAlias {
    aggregate_id: AudioObjectID,
    /// Name of the aggregate (what cpal enumerates).
    pub device_name: String,
}

impl DeviceAlias {
    /// Wrap the device with `uid`. `Err(DeviceNotFound)` if no such device exists.
    pub fn new(uid: &str, label: &str) -> Result<Self> {
        if device_for_uid(uid).is_none() {
            return Err(YetiError::DeviceNotFound);
        }
        let pid = std::process::id();
        let n = INSTANCE.fetch_add(1, Ordering::Relaxed);
        let device_name = format!("{label} [Yeti Studio {pid}.{n}]");
        let sub: Retained<NSDictionary<NSString, NSObject>> = NSDictionary::from_retained_objects(
            &[&*key(kAudioSubDeviceUIDKey)],
            &[Retained::into_super(NSString::from_str(uid))],
        );
        let subs = NSArray::from_retained_slice(&[sub]);
        let props: Retained<NSDictionary<NSString, NSObject>> = NSDictionary::from_retained_objects(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceSubDeviceListKey),
                &*key(kAudioAggregateDeviceMainSubDeviceKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
            ],
            &[
                Retained::into_super(NSString::from_str(&device_name)),
                Retained::into_super(NSString::from_str(&format!(
                    "com.yetistudio.alias.{pid}.{n}"
                ))),
                Retained::into_super(subs),
                Retained::into_super(NSString::from_str(uid)),
                Retained::into_super(Retained::into_super(NSNumber::new_bool(true))),
            ],
        );
        let mut aggregate_id: AudioObjectID = 0;
        // SAFETY: NSDictionary is toll-free bridged to CFDictionary; it outlives the call.
        let st = unsafe {
            let cf = &*(Retained::as_ptr(&props) as *const CFDictionary);
            AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate_id))
        };
        if st != 0 {
            return Err(YetiError::Backend(format!(
                "aggregate for {uid}: AudioHardwareCreateAggregateDevice failed (OSStatus {st})"
            )));
        }
        Ok(Self {
            aggregate_id,
            device_name,
        })
    }
}

impl Drop for DeviceAlias {
    fn drop(&mut self) {
        // SAFETY: created by this object; status ignored in drop.
        unsafe {
            AudioHardwareDestroyAggregateDevice(self.aggregate_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_uid_is_not_found() {
        assert!(device_for_uid("YetiStudio_no_such_device_UID").is_none());
        assert!(matches!(
            DeviceAlias::new("YetiStudio_no_such_device_UID", "nothing"),
            Err(YetiError::DeviceNotFound)
        ));
    }

    /// Wraps the system default output in an alias and checks cpal can see it and build
    /// (not play) an output stream on it. Touches real audio hardware, so it is ignored by
    /// default: `cargo test -p yeti-engine --lib mac_feed -- --ignored`.
    #[test]
    #[ignore]
    fn alias_of_default_output_is_visible_to_cpal() {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let dev = host.default_output_device().expect("default output");
        let uid = dev.id().expect("uid").id().to_string();
        assert!(device_for_uid(&uid).is_some());
        let alias = DeviceAlias::new(&uid, "alias test").expect("aggregate");
        let found = crate::pipeline::find_device(&host, &alias.device_name, false)
            .expect("alias visible to cpal");
        let cfg = found.default_output_config().expect("config");
        let stream = found
            .build_output_stream::<f32, _, _>(
                cfg.config(),
                |d: &mut [f32], _| d.fill(0.0),
                |e| panic!("{e}"),
                None,
            )
            .expect("build stream");
        drop(stream);
        let name = alias.device_name.clone();
        drop(alias);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(crate::pipeline::find_device(&host, &name, false).is_none());
    }
}
