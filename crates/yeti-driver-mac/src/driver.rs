//! AudioServerPlugIn driver: CFPlugIn factory, COM vtable and the object/property model.
//!
//! Object tree (fixed, no dynamic devices):
//! ```text
//! 1 plug-in
//! ├── 2 device "Yeti Studio Mic"         (input only, visible, can be default input)
//! │   └── 3 input stream  (2 ch, 48 kHz, f32)
//! └── 4 device "Yeti Studio Mic (Feed)"  (output only, IsHidden=1, CanBeDefault*=0)
//!     └── 5 output stream (2 ch, 48 kHz, f32)
//! ```
//! Audio the engine writes to the Feed (WriteMix) is read back by the Mic (ReadInput) through
//! the lock-free ring in [`crate::loopback`].
//!
//! Every exported / vtable entry point runs inside `catch_unwind` ([`guard`]); a panic turns
//! into an error status instead of unwinding into coreaudiod.
#![allow(non_upper_case_globals)] // CoreAudio constant names are used as match patterns.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use crate::ffi::*;
use crate::loopback::{self, CHANNELS, SAMPLE_RATE, ZTS_PERIOD};
use crate::{
    FEED_DEVICE_NAME, FEED_DEVICE_UID, MANUFACTURER, MIC_DEVICE_NAME, MIC_DEVICE_UID, MODEL_UID,
};

// ---------------------------------------------------------------------------------------------
// Object model
// ---------------------------------------------------------------------------------------------

pub const PLUGIN_ID: AudioObjectID = kAudioObjectPlugInObject;
pub const MIC_DEVICE_ID: AudioObjectID = 2;
pub const MIC_STREAM_ID: AudioObjectID = 3;
pub const FEED_DEVICE_ID: AudioObjectID = 4;
pub const FEED_STREAM_ID: AudioObjectID = 5;

/// Both devices share one clock (see loopback.rs), so they report the same clock domain.
const CLOCK_DOMAIN: u32 = fourcc(b"YETI");

static DEVICE_IDS: [AudioObjectID; 2] = [MIC_DEVICE_ID, FEED_DEVICE_ID];
static MIC_STREAMS: [AudioObjectID; 1] = [MIC_STREAM_ID];
static FEED_STREAMS: [AudioObjectID; 1] = [FEED_STREAM_ID];
static NO_IDS: [AudioObjectID; 0] = [];
static MIC_RELATED: [AudioObjectID; 1] = [MIC_DEVICE_ID];
static FEED_RELATED: [AudioObjectID; 1] = [FEED_DEVICE_ID];

struct Device {
    index: usize,
    id: AudioObjectID,
    stream: AudioObjectID,
    name: &'static str,
    uid: &'static str,
    is_input: bool,
    hidden: u32,
    can_be_default: u32,
    can_be_default_system: u32,
    terminal: u32,
    streams: &'static [AudioObjectID],
    related: &'static [AudioObjectID],
}

static MIC: Device = Device {
    index: 0,
    id: MIC_DEVICE_ID,
    stream: MIC_STREAM_ID,
    name: MIC_DEVICE_NAME,
    uid: MIC_DEVICE_UID,
    is_input: true,
    hidden: 0,
    can_be_default: 1,
    can_be_default_system: 0,
    terminal: kAudioStreamTerminalTypeMicrophone,
    streams: &MIC_STREAMS,
    related: &MIC_RELATED,
};

static FEED: Device = Device {
    index: 1,
    id: FEED_DEVICE_ID,
    stream: FEED_STREAM_ID,
    name: FEED_DEVICE_NAME,
    uid: FEED_DEVICE_UID,
    is_input: false,
    hidden: 1,
    can_be_default: 0,
    can_be_default_system: 0,
    terminal: kAudioStreamTerminalTypeLine,
    streams: &FEED_STREAMS,
    related: &FEED_RELATED,
};

fn device(id: AudioObjectID) -> Option<&'static Device> {
    match id {
        MIC_DEVICE_ID => Some(&MIC),
        FEED_DEVICE_ID => Some(&FEED),
        _ => None,
    }
}

fn stream_device(id: AudioObjectID) -> Option<&'static Device> {
    match id {
        MIC_STREAM_ID => Some(&MIC),
        FEED_STREAM_ID => Some(&FEED),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Mutable state (atomics only; START_STOP serialises StartIO/StopIO, never taken on IO paths)
// ---------------------------------------------------------------------------------------------

static REF_COUNT: AtomicU32 = AtomicU32::new(1);
static HOST: AtomicPtr<AudioServerPlugInHostInterface> = AtomicPtr::new(ptr::null_mut());
static RUNNING: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
static STREAM_ACTIVE: [AtomicU32; 2] = [AtomicU32::new(1), AtomicU32::new(1)];
static START_STOP: Mutex<()> = Mutex::new(());

fn stream_format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: SAMPLE_RATE,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: (4 * CHANNELS) as u32,
        mFramesPerPacket: 1,
        mBytesPerFrame: (4 * CHANNELS) as u32,
        mChannelsPerFrame: CHANNELS as u32,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn sample_rate_range() -> AudioValueRange {
    AudioValueRange {
        mMinimum: SAMPLE_RATE,
        mMaximum: SAMPLE_RATE,
    }
}

fn stereo_layout() -> AudioChannelLayoutStereo {
    let mut l = AudioChannelLayoutStereo {
        mChannelLayoutTag: kAudioChannelLayoutTag_UseChannelDescriptions,
        mChannelBitmap: 0,
        mNumberChannelDescriptions: 2,
        ..Default::default()
    };
    l.mChannelDescriptions[0].mChannelLabel = kAudioChannelLabel_Left;
    l.mChannelDescriptions[1].mChannelLabel = kAudioChannelLabel_Right;
    l
}

// ---------------------------------------------------------------------------------------------
// Property values
// ---------------------------------------------------------------------------------------------

enum Val {
    U32(u32),
    F64(f64),
    /// Returned as a new +1 CFStringRef (the caller releases it).
    Str(&'static str),
    /// Variable-length array of AudioObjectIDs (truncated to the caller's buffer).
    Ids(&'static [AudioObjectID]),
    U32Pair([u32; 2]),
    Asbd(AudioStreamBasicDescription),
    /// One-element arrays (truncated to the caller's buffer, like `Ids`).
    Ranged(AudioStreamRangedDescription),
    Range(AudioValueRange),
    Layout(AudioChannelLayoutStereo),
}

impl Val {
    fn size(&self) -> u32 {
        (match self {
            Val::U32(_) => size_of::<u32>(),
            Val::F64(_) => size_of::<f64>(),
            Val::Str(_) => size_of::<CFStringRef>(),
            Val::Ids(ids) => size_of_val(*ids),
            Val::U32Pair(_) => size_of::<[u32; 2]>(),
            Val::Asbd(_) => size_of::<AudioStreamBasicDescription>(),
            Val::Ranged(_) => size_of::<AudioStreamRangedDescription>(),
            Val::Range(_) => size_of::<AudioValueRange>(),
            Val::Layout(_) => size_of::<AudioChannelLayoutStereo>(),
        }) as u32
    }

    /// Writes the value into `out` (capacity `cap` bytes); returns bytes written.
    ///
    /// # Safety
    /// `out` must be valid for `cap` bytes of writes.
    unsafe fn write(&self, out: *mut c_void, cap: u32) -> Result<u32, OSStatus> {
        if out.is_null() {
            return Err(kAudioHardwareIllegalOperationError);
        }
        // Arrays are truncated to whole elements; scalars need the full size.
        let (elem, count) = match self {
            Val::Ids(ids) => (size_of::<AudioObjectID>() as u32, ids.len() as u32),
            Val::Ranged(_) | Val::Range(_) => (self.size(), 1),
            _ => {
                if cap < self.size() {
                    return Err(kAudioHardwareBadPropertySizeError);
                }
                (self.size(), 1)
            }
        };
        let n = count.min(cap / elem.max(1));
        // SAFETY: caller guarantees `out` is writable for `cap` >= written bytes; unaligned
        // writes because the HAL only guarantees byte alignment in theory.
        unsafe {
            match self {
                Val::U32(v) => ptr::write_unaligned(out.cast::<u32>(), *v),
                Val::F64(v) => ptr::write_unaligned(out.cast::<f64>(), *v),
                Val::Str(s) => {
                    let cf = cfstring(s);
                    if cf.is_null() {
                        return Err(kAudioHardwareUnspecifiedError);
                    }
                    ptr::write_unaligned(out.cast::<CFStringRef>(), cf);
                }
                Val::Ids(ids) => ptr::copy_nonoverlapping(
                    ids.as_ptr().cast::<u8>(),
                    out.cast::<u8>(),
                    (n * elem) as usize,
                ),
                Val::U32Pair(v) => ptr::write_unaligned(out.cast::<[u32; 2]>(), *v),
                Val::Asbd(v) => ptr::write_unaligned(out.cast(), *v),
                Val::Ranged(v) if n == 1 => ptr::write_unaligned(out.cast(), *v),
                Val::Range(v) if n == 1 => ptr::write_unaligned(out.cast(), *v),
                Val::Ranged(_) | Val::Range(_) => {}
                Val::Layout(v) => ptr::write_unaligned(out.cast(), *v),
            }
        }
        Ok(n * elem)
    }
}

/// Qualifier passed with a property query.
#[derive(Clone, Copy)]
struct Qual {
    size: u32,
    data: *const c_void,
}

/// The property model. `Err(kAudioHardwareUnknownPropertyError)` = HasProperty false.
fn property(
    obj: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    qual: Qual,
) -> Result<Val, OSStatus> {
    let sel = addr.mSelector;
    let unknown = Err(kAudioHardwareUnknownPropertyError);
    if obj == PLUGIN_ID {
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(Val::U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(Val::U32(kAudioPlugInClassID)),
            kAudioObjectPropertyOwner => Ok(Val::U32(kAudioObjectUnknown)),
            kAudioObjectPropertyManufacturer => Ok(Val::Str(MANUFACTURER)),
            kAudioObjectPropertyOwnedObjects | kAudioPlugInPropertyDeviceList => {
                Ok(Val::Ids(&DEVICE_IDS))
            }
            kAudioPlugInPropertyTranslateUIDToDevice => Ok(Val::U32(translate_uid(qual))),
            kAudioPlugInPropertyResourceBundle => Ok(Val::Str("")),
            _ => unknown,
        };
    }
    if let Some(d) = device(obj) {
        let scoped_streams = match addr.mScope {
            kAudioObjectPropertyScopeInput if !d.is_input => &NO_IDS[..],
            kAudioObjectPropertyScopeOutput if d.is_input => &NO_IDS[..],
            _ => d.streams,
        };
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(Val::U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(Val::U32(kAudioDeviceClassID)),
            kAudioObjectPropertyOwner => Ok(Val::U32(PLUGIN_ID)),
            kAudioObjectPropertyName => Ok(Val::Str(d.name)),
            kAudioObjectPropertyManufacturer => Ok(Val::Str(MANUFACTURER)),
            kAudioObjectPropertyOwnedObjects | kAudioDevicePropertyStreams => {
                Ok(Val::Ids(scoped_streams))
            }
            kAudioObjectPropertyControlList => Ok(Val::Ids(&NO_IDS)),
            kAudioDevicePropertyDeviceUID => Ok(Val::Str(d.uid)),
            kAudioDevicePropertyModelUID => Ok(Val::Str(MODEL_UID)),
            kAudioDevicePropertyTransportType => Ok(Val::U32(kAudioDeviceTransportTypeVirtual)),
            kAudioDevicePropertyRelatedDevices => Ok(Val::Ids(d.related)),
            kAudioDevicePropertyClockDomain => Ok(Val::U32(CLOCK_DOMAIN)),
            kAudioDevicePropertyDeviceIsAlive => Ok(Val::U32(1)),
            kAudioDevicePropertyDeviceIsRunning => Ok(Val::U32(u32::from(
                RUNNING[d.index].load(Ordering::Relaxed) > 0,
            ))),
            kAudioDevicePropertyDeviceCanBeDefaultDevice => Ok(Val::U32(d.can_be_default)),
            kAudioDevicePropertyDeviceCanBeDefaultSystemDevice => {
                Ok(Val::U32(d.can_be_default_system))
            }
            kAudioDevicePropertyLatency | kAudioDevicePropertySafetyOffset => Ok(Val::U32(0)),
            kAudioDevicePropertyNominalSampleRate => Ok(Val::F64(SAMPLE_RATE)),
            kAudioDevicePropertyAvailableNominalSampleRates => Ok(Val::Range(sample_rate_range())),
            kAudioDevicePropertyIsHidden => Ok(Val::U32(d.hidden)),
            kAudioDevicePropertyPreferredChannelsForStereo => Ok(Val::U32Pair([1, 2])),
            kAudioDevicePropertyPreferredChannelLayout => Ok(Val::Layout(stereo_layout())),
            kAudioDevicePropertyZeroTimeStampPeriod => Ok(Val::U32(ZTS_PERIOD)),
            _ => unknown,
        };
    }
    if let Some(d) = stream_device(obj) {
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(Val::U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(Val::U32(kAudioStreamClassID)),
            kAudioObjectPropertyOwner => Ok(Val::U32(d.id)),
            kAudioObjectPropertyOwnedObjects => Ok(Val::Ids(&NO_IDS)),
            kAudioStreamPropertyIsActive => {
                Ok(Val::U32(STREAM_ACTIVE[d.index].load(Ordering::Relaxed)))
            }
            kAudioStreamPropertyDirection => Ok(Val::U32(u32::from(d.is_input))),
            kAudioStreamPropertyTerminalType => Ok(Val::U32(d.terminal)),
            kAudioStreamPropertyStartingChannel => Ok(Val::U32(1)),
            kAudioStreamPropertyLatency => Ok(Val::U32(0)),
            kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat => {
                Ok(Val::Asbd(stream_format()))
            }
            kAudioStreamPropertyAvailableVirtualFormats
            | kAudioStreamPropertyAvailablePhysicalFormats => {
                Ok(Val::Ranged(AudioStreamRangedDescription {
                    mFormat: stream_format(),
                    mSampleRateRange: sample_rate_range(),
                }))
            }
            _ => unknown,
        };
    }
    Err(kAudioHardwareBadObjectError)
}

fn is_settable(obj: AudioObjectID, sel: u32) -> bool {
    stream_device(obj).is_some()
        && matches!(
            sel,
            kAudioStreamPropertyIsActive
                | kAudioStreamPropertyVirtualFormat
                | kAudioStreamPropertyPhysicalFormat
        )
}

fn translate_uid(qual: Qual) -> AudioObjectID {
    if qual.data.is_null() || (qual.size as usize) < size_of::<CFStringRef>() {
        return kAudioObjectUnknown;
    }
    // SAFETY: the HAL passes a CFStringRef as the qualifier for this selector.
    let s = unsafe { ptr::read_unaligned(qual.data.cast::<CFStringRef>()) };
    let mut buf = [0u8; 256];
    // SAFETY: `s` is a CFStringRef (or NULL, handled inside).
    match unsafe { cfstring_to_utf8(s, &mut buf) } {
        Some(b) if b == MIC_DEVICE_UID.as_bytes() => MIC_DEVICE_ID,
        Some(b) if b == FEED_DEVICE_UID.as_bytes() => FEED_DEVICE_ID,
        _ => kAudioObjectUnknown,
    }
}

fn notify(obj: AudioObjectID, sel: u32) {
    let host = HOST.load(Ordering::Acquire);
    if host.is_null() {
        return;
    }
    let addr = AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    // SAFETY: `host` is the interface the HAL passed to Initialize and outlives the driver.
    unsafe { ((*host).PropertiesChanged)(host, obj, 1, &addr) };
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// Runs `f`, converting a panic into `on_panic` (never unwinds across the FFI boundary).
#[inline]
fn guard<R>(on_panic: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(on_panic)
}

const PANIC_STATUS: OSStatus = kAudioHardwareUnspecifiedError;

fn check_driver(drv: DrvRef) -> Result<(), OSStatus> {
    if drv.cast::<c_void>() == driver_ref() {
        Ok(())
    } else {
        Err(kAudioHardwareBadObjectError)
    }
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
}

unsafe extern "C" fn query_interface(
    drv: *mut c_void,
    uuid: CFUUIDBytes,
    out: *mut *mut c_void,
) -> HRESULT {
    guard(E_UNEXPECTED, || {
        if drv != driver_ref() || out.is_null() {
            return E_POINTER;
        }
        if uuid == IUNKNOWN_UUID || uuid == AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID {
            REF_COUNT.fetch_add(1, Ordering::AcqRel);
            // SAFETY: `out` checked non-null; the HAL provides a writable pointer slot.
            unsafe { *out = driver_ref() };
            S_OK
        } else {
            // SAFETY: as above.
            unsafe { *out = ptr::null_mut() };
            E_NOINTERFACE
        }
    })
}

unsafe extern "C" fn add_ref(drv: *mut c_void) -> ULONG {
    guard(0, || {
        if drv != driver_ref() {
            return 0;
        }
        REF_COUNT.fetch_add(1, Ordering::AcqRel).saturating_add(1)
    })
}

unsafe extern "C" fn release(drv: *mut c_void) -> ULONG {
    guard(0, || {
        if drv != driver_ref() {
            return 0;
        }
        // The driver is static; the count is informational and never goes below zero.
        match REF_COUNT.try_update(Ordering::AcqRel, Ordering::Acquire, |c| c.checked_sub(1)) {
            Ok(prev) => prev - 1,
            Err(_) => 0,
        }
    })
}

unsafe extern "C" fn initialize(drv: DrvRef, host: AudioServerPlugInHostRef) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        HOST.store(host.cast_mut(), Ordering::Release);
        loopback::reset_clock_and_ring();
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn create_device(
    drv: DrvRef,
    _desc: CFDictionaryRef,
    _client: *const AudioServerPlugInClientInfo,
    _out: *mut AudioObjectID,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        kAudioHardwareUnsupportedOperationError
    })
}

unsafe extern "C" fn destroy_device(drv: DrvRef, _dev: AudioObjectID) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        kAudioHardwareUnsupportedOperationError
    })
}

unsafe extern "C" fn device_client(
    drv: DrvRef,
    dev: AudioObjectID,
    _client: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if device(dev).is_none() {
            return kAudioHardwareBadObjectError;
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn config_change(
    drv: DrvRef,
    dev: AudioObjectID,
    _action: u64,
    _info: *mut c_void,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if device(dev).is_none() {
            return kAudioHardwareBadObjectError;
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn has_property(
    drv: DrvRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: Addr,
) -> Boolean {
    guard(0, || {
        if check_driver(drv).is_err() || addr.is_null() {
            return 0;
        }
        // SAFETY: non-null address provided by the HAL.
        let a = unsafe { ptr::read_unaligned(addr) };
        u8::from(
            property(
                obj,
                &a,
                Qual {
                    size: 0,
                    data: ptr::null(),
                },
            )
            .is_ok(),
        )
    })
}

unsafe extern "C" fn is_property_settable(
    drv: DrvRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: Addr,
    out: *mut Boolean,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if addr.is_null() || out.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        // SAFETY: non-null pointers provided by the HAL.
        let a = unsafe { ptr::read_unaligned(addr) };
        tri!(
            property(
                obj,
                &a,
                Qual {
                    size: 0,
                    data: ptr::null()
                }
            )
            .map(|_| ())
        );
        // SAFETY: as above.
        unsafe { *out = u8::from(is_settable(obj, a.mSelector)) };
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn get_property_data_size(
    drv: DrvRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: Addr,
    qual_size: u32,
    qual: *const c_void,
    out_size: *mut u32,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if addr.is_null() || out_size.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        // SAFETY: non-null pointers provided by the HAL.
        let a = unsafe { ptr::read_unaligned(addr) };
        let v = tri!(property(
            obj,
            &a,
            Qual {
                size: qual_size,
                data: qual
            }
        ));
        // SAFETY: as above.
        unsafe { *out_size = v.size() };
        kAudioHardwareNoError
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn get_property_data(
    drv: DrvRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: Addr,
    qual_size: u32,
    qual: *const c_void,
    in_size: u32,
    out_size: *mut u32,
    out: *mut c_void,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if addr.is_null() || out_size.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        // SAFETY: non-null pointers provided by the HAL.
        let a = unsafe { ptr::read_unaligned(addr) };
        let v = tri!(property(
            obj,
            &a,
            Qual {
                size: qual_size,
                data: qual
            }
        ));
        // SAFETY: the HAL guarantees `out` is writable for `in_size` bytes.
        let written = tri!(unsafe { v.write(out, in_size) });
        // SAFETY: checked non-null.
        unsafe { *out_size = written };
        kAudioHardwareNoError
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn set_property_data(
    drv: DrvRef,
    obj: AudioObjectID,
    _pid: pid_t,
    addr: Addr,
    _qual_size: u32,
    _qual: *const c_void,
    in_size: u32,
    data: *const c_void,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if addr.is_null() || data.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        // SAFETY: non-null pointer provided by the HAL.
        let a = unsafe { ptr::read_unaligned(addr) };
        tri!(
            property(
                obj,
                &a,
                Qual {
                    size: 0,
                    data: ptr::null()
                }
            )
            .map(|_| ())
        );
        if !is_settable(obj, a.mSelector) {
            return kAudioHardwareUnknownPropertyError;
        }
        let Some(d) = stream_device(obj) else {
            return kAudioHardwareBadObjectError;
        };
        match a.mSelector {
            kAudioStreamPropertyIsActive => {
                if (in_size as usize) < size_of::<u32>() {
                    return kAudioHardwareBadPropertySizeError;
                }
                // SAFETY: `data` is readable for `in_size` >= 4 bytes.
                let v = u32::from(unsafe { ptr::read_unaligned(data.cast::<u32>()) } != 0);
                if STREAM_ACTIVE[d.index].swap(v, Ordering::Relaxed) != v {
                    notify(obj, kAudioStreamPropertyIsActive);
                }
                kAudioHardwareNoError
            }
            _ => {
                // Virtual / physical format: only the one fixed format is accepted.
                if (in_size as usize) < size_of::<AudioStreamBasicDescription>() {
                    return kAudioHardwareBadPropertySizeError;
                }
                // SAFETY: `data` is readable for a full ASBD.
                let f = unsafe { ptr::read_unaligned(data.cast::<AudioStreamBasicDescription>()) };
                if f == stream_format() {
                    kAudioHardwareNoError
                } else {
                    kAudioDeviceUnsupportedFormatError
                }
            }
        }
    })
}

unsafe extern "C" fn start_io(drv: DrvRef, dev: AudioObjectID, _client: u32) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        let Some(d) = device(dev) else {
            return kAudioHardwareBadObjectError;
        };
        let became_running = {
            let _g = START_STOP.lock().unwrap_or_else(|e| e.into_inner());
            let any_running = RUNNING.iter().any(|r| r.load(Ordering::Relaxed) > 0);
            if !any_running {
                loopback::reset_clock_and_ring();
            }
            RUNNING[d.index].fetch_add(1, Ordering::AcqRel) == 0
        };
        if became_running {
            notify(dev, kAudioDevicePropertyDeviceIsRunning);
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn stop_io(drv: DrvRef, dev: AudioObjectID, _client: u32) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        let Some(d) = device(dev) else {
            return kAudioHardwareBadObjectError;
        };
        let stopped = {
            let _g = START_STOP.lock().unwrap_or_else(|e| e.into_inner());
            match RUNNING[d.index]
                .try_update(Ordering::AcqRel, Ordering::Acquire, |c| c.checked_sub(1))
            {
                Ok(prev) => prev == 1,
                Err(_) => return kAudioHardwareNotRunningError,
            }
        };
        if stopped {
            notify(dev, kAudioDevicePropertyDeviceIsRunning);
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn get_zero_time_stamp(
    drv: DrvRef,
    dev: AudioObjectID,
    _client: u32,
    out_sample_time: *mut f64,
    out_host_time: *mut u64,
    out_seed: *mut u64,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if device(dev).is_none() {
            return kAudioHardwareBadObjectError;
        }
        if out_sample_time.is_null() || out_host_time.is_null() || out_seed.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        let (s, h, seed) = loopback::zero_timestamp();
        // SAFETY: non-null out-pointers provided by the HAL.
        unsafe {
            *out_sample_time = s;
            *out_host_time = h;
            *out_seed = seed;
        }
        kAudioHardwareNoError
    })
}

/// The single IO operation each device performs.
fn device_op(d: &Device) -> u32 {
    if d.is_input {
        kAudioServerPlugInIOOperationReadInput
    } else {
        kAudioServerPlugInIOOperationWriteMix
    }
}

unsafe extern "C" fn will_do_io_operation(
    drv: DrvRef,
    dev: AudioObjectID,
    _client: u32,
    op: u32,
    out_will_do: *mut Boolean,
    out_in_place: *mut Boolean,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        let Some(d) = device(dev) else {
            return kAudioHardwareBadObjectError;
        };
        let will = op == device_op(d);
        // SAFETY: out-pointers provided by the HAL (checked for NULL).
        unsafe {
            if !out_will_do.is_null() {
                *out_will_do = u8::from(will);
            }
            if !out_in_place.is_null() {
                *out_in_place = 1;
            }
        }
        kAudioHardwareNoError
    })
}

unsafe extern "C" fn begin_end_io_operation(
    drv: DrvRef,
    dev: AudioObjectID,
    _client: u32,
    _op: u32,
    _frames: u32,
    _cycle: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        if device(dev).is_none() {
            return kAudioHardwareBadObjectError;
        }
        kAudioHardwareNoError
    })
}

/// Real-time: no allocation, no locks — only the atomic ring in `loopback`.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn do_io_operation(
    drv: DrvRef,
    dev: AudioObjectID,
    stream: AudioObjectID,
    _client: u32,
    op: u32,
    frames: u32,
    cycle: *const AudioServerPlugInIOCycleInfo,
    main_buf: *mut c_void,
    _secondary: *mut c_void,
) -> OSStatus {
    guard(PANIC_STATUS, || {
        tri!(check_driver(drv));
        let Some(d) = device(dev) else {
            return kAudioHardwareBadObjectError;
        };
        if stream != d.stream {
            return kAudioHardwareBadStreamError;
        }
        if op != device_op(d) {
            return kAudioHardwareNoError;
        }
        if cycle.is_null() || main_buf.is_null() {
            return kAudioHardwareIllegalOperationError;
        }
        let len = frames as usize * CHANNELS;
        // SAFETY: the HAL passes a valid cycle-info pointer and an interleaved f32 buffer of
        // `frames` frames in our (fixed) virtual format.
        unsafe {
            let cycle = &*cycle;
            if d.is_input {
                let out = std::slice::from_raw_parts_mut(main_buf.cast::<f32>(), len);
                loopback::read(cycle.mInputTime.mSampleTime.floor() as i64, out);
            } else {
                let data = std::slice::from_raw_parts(main_buf.cast::<f32>(), len);
                loopback::write(cycle.mOutputTime.mSampleTime.floor() as i64, data);
            }
        }
        kAudioHardwareNoError
    })
}

// ---------------------------------------------------------------------------------------------
// vtable + factory
// ---------------------------------------------------------------------------------------------

#[repr(transparent)]
struct VTable(AudioServerPlugInDriverInterface);
// SAFETY: immutable table of function pointers (+ a NULL reserved field).
unsafe impl Sync for VTable {}

#[repr(transparent)]
struct DriverRefCell(*const AudioServerPlugInDriverInterface);
// SAFETY: immutable pointer to an immutable static.
unsafe impl Sync for DriverRefCell {}

static VTABLE: VTable = VTable(AudioServerPlugInDriverInterface {
    _reserved: ptr::null_mut(),
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
    Initialize: initialize,
    CreateDevice: create_device,
    DestroyDevice: destroy_device,
    AddDeviceClient: device_client,
    RemoveDeviceClient: device_client,
    PerformDeviceConfigurationChange: config_change,
    AbortDeviceConfigurationChange: config_change,
    HasProperty: has_property,
    IsPropertySettable: is_property_settable,
    GetPropertyDataSize: get_property_data_size,
    GetPropertyData: get_property_data,
    SetPropertyData: set_property_data,
    StartIO: start_io,
    StopIO: stop_io,
    GetZeroTimeStamp: get_zero_time_stamp,
    WillDoIOOperation: will_do_io_operation,
    BeginIOOperation: begin_end_io_operation,
    DoIOOperation: do_io_operation,
    EndIOOperation: begin_end_io_operation,
});

/// `AudioServerPlugInDriverRef` = pointer to (pointer to vtable).
static DRIVER: DriverRefCell = DriverRefCell(&VTABLE.0);

/// The driver reference handed to the HAL.
pub fn driver_ref() -> *mut c_void {
    (&raw const DRIVER).cast_mut().cast()
}

/// CFPlugIn factory named in Info.plist (`CFPlugInFactories`).
/// Returns the driver for `kAudioServerPlugInTypeUUID`, NULL for any other type.
///
/// # Safety
/// `requested_type_uuid` must be NULL or a valid `CFUUIDRef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn YetiStudioMic_Create(
    _allocator: CFAllocatorRef,
    requested_type_uuid: CFUUIDRef,
) -> *mut c_void {
    guard(ptr::null_mut(), || {
        if requested_type_uuid.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: valid CFUUIDRef per the caller contract.
        let bytes = unsafe { CFUUIDGetUUIDBytes(requested_type_uuid) };
        if bytes == AUDIO_SERVER_PLUGIN_TYPE_UUID {
            driver_ref()
        } else {
            ptr::null_mut()
        }
    })
}
