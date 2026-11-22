//! Self-excluding system-audio tap for the AEC far-end reference (macOS 14.2+).
//!
//! cpal 0.18's built-in loopback taps *all* processes, so our own monitor ("hear myself")
//! output would leak into the echo-cancel reference whenever the monitor device is also the
//! system output. This creates a global stereo Core Audio process tap that **excludes this
//! process**, wraps it in a private aggregate device, and lets cpal capture from that device
//! by name. Needs the "System Audio Recording" (TCC) permission; without it the tap delivers
//! silence.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};
use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use yeti_types::{Result, YetiError};

static INSTANCE: AtomicU32 = AtomicU32::new(0);

fn os(what: &str, st: i32) -> YetiError {
    YetiError::Backend(format!("process tap: {what} failed (OSStatus {st})"))
}

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(k.to_str().unwrap_or_default())
}

/// HAL process object of this process (`kAudioObjectUnknown` = 0 if not registered yet).
fn own_process_object() -> AudioObjectID {
    let pid: i32 = std::process::id() as i32;
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut obj: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: qualifier = pid_t, out = AudioObjectID; all pointers valid for the call.
    let st = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&addr),
            size_of::<i32>() as u32,
            (&pid as *const i32).cast(),
            NonNull::from(&mut size),
            NonNull::from(&mut obj).cast(),
        )
    };
    if st != 0 { 0 } else { obj }
}

/// A live tap + private aggregate device; both are destroyed on drop.
pub struct ProcessTap {
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    /// Name of the aggregate device (what cpal enumerates).
    pub device_name: String,
    /// Whether our own process object was found and excluded.
    pub excludes_self: bool,
}

impl ProcessTap {
    pub fn new_excluding_self() -> Result<Self> {
        let me = own_process_object();
        let pid = std::process::id();
        let n = INSTANCE.fetch_add(1, Ordering::Relaxed);
        let excl: Vec<Retained<NSNumber>> = if me != 0 {
            vec![NSNumber::new_u32(me)]
        } else {
            vec![]
        };
        let excl = NSArray::from_retained_slice(&excl);
        // SAFETY: standard alloc/init of an Objective-C object; setters take valid values.
        let desc = unsafe {
            let d = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &excl,
            );
            d.setName(&NSString::from_str(&format!(
                "Yeti Studio reference tap {pid}.{n}"
            )));
            d.setPrivate(true);
            d.setMuteBehavior(CATapMuteBehavior::Unmuted);
            d
        };
        let mut tap_id: AudioObjectID = 0;
        // SAFETY: valid description and out-pointer.
        let st = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id) };
        if st != 0 {
            return Err(os("AudioHardwareCreateProcessTap", st));
        }
        // SAFETY: plain getter on a live object.
        let tap_uid = unsafe { desc.UUID().UUIDString() };

        let device_name = format!("Yeti Studio reference {pid}.{n}");
        let sub_tap: Retained<NSDictionary<NSString, NSObject>> =
            NSDictionary::from_retained_objects(
                &[
                    &*key(kAudioSubTapUIDKey),
                    &*key(kAudioSubTapDriftCompensationKey),
                ],
                &[
                    Retained::into_super(tap_uid),
                    Retained::into_super(Retained::into_super(NSNumber::new_bool(true))),
                ],
            );
        let taps = NSArray::from_retained_slice(&[sub_tap]);
        let props: Retained<NSDictionary<NSString, NSObject>> = NSDictionary::from_retained_objects(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceTapListKey),
                &*key(kAudioAggregateDeviceTapAutoStartKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
            ],
            &[
                Retained::into_super(NSString::from_str(&device_name)),
                Retained::into_super(NSString::from_str(&format!(
                    "com.yetistudio.reference.{pid}.{n}"
                ))),
                Retained::into_super(taps),
                Retained::into_super(Retained::into_super(NSNumber::new_bool(true))),
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
            // SAFETY: tap was created above.
            unsafe { AudioHardwareDestroyProcessTap(tap_id) };
            return Err(os("AudioHardwareCreateAggregateDevice", st));
        }
        Ok(Self {
            tap_id,
            aggregate_id,
            device_name,
            excludes_self: me != 0,
        })
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        // SAFETY: ids were created by this object; statuses ignored in drop.
        unsafe {
            AudioHardwareDestroyAggregateDevice(self.aggregate_id);
            AudioHardwareDestroyProcessTap(self.tap_id);
        }
    }
}
