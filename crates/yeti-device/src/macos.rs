//! macOS backend: CoreAudio HAL properties via `objc2-core-audio`.
//!
//! The Yeti enumerates as **two** HAL devices with the same name ("Yeti Stereo Microphone"):
//! one with input streams (capture, gain, mute) and one with output streams (headphone jack).
//!
//! - input gain: `kAudioDevicePropertyVolumeDecibels`, scope input, per channel element (1, 2)
//!   or the main element (0) when the device exposes one
//! - mute: `kAudioDevicePropertyMute` (element 0 on the Yeti)
//! - headphone volume: `kAudioDevicePropertyVolumeScalar`, scope output
//! - rate: `kAudioDevicePropertyNominalSampleRate` / `...AvailableNominalSampleRates`
//! - hot-plug: listener on `kAudioHardwarePropertyDevices` (system object); external changes:
//!   listeners on the Yeti's mute/volume/rate properties → [`DeviceEvent::Changed`].
//!
//! Listener callbacks run on a CoreAudio notification thread. They re-query state (HAL IPC)
//! and invoke the user callback outside of any internal lock.

use crate::{DeviceControl, DeviceEventCallback, YETI_NAME};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectHasProperty, AudioObjectID, AudioObjectIsPropertySettable,
    AudioObjectPropertyAddress, AudioObjectRemovePropertyListener, AudioObjectSetPropertyData,
    kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyMute, kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreams,
    kAudioDevicePropertyTransportType, kAudioDevicePropertyVolumeDecibels,
    kAudioDevicePropertyVolumeRangeDecibels, kAudioDevicePropertyVolumeScalar,
    kAudioDeviceTransportTypeUSB, kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput,
};
use objc2_core_foundation::{CFRetained, CFString};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use yeti_types::{DeviceEvent, DeviceState, Result, YetiError};

/// The HAL system object (root of device enumeration).
pub const SYSTEM_OBJECT: u32 = objc2_core_audio::kAudioObjectSystemObject as u32;

const ELEMENT_WILDCARD: u32 = 0xFFFF_FFFF;

/// `AudioValueRange` (CoreAudioTypes), declared locally to avoid another dependency.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ValueRange {
    min: f64,
    max: f64,
}

// ---------------------------------------------------------------------------
// Thin, safe property helpers
// ---------------------------------------------------------------------------

fn addr(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    }
}

fn os_err(what: &str, status: i32) -> YetiError {
    let b = status.to_be_bytes();
    let fourcc = if b.iter().all(|c| c.is_ascii_graphic()) {
        format!(" '{}'", String::from_utf8_lossy(&b))
    } else {
        String::new()
    };
    YetiError::Backend(format!("CoreAudio {what}: OSStatus {status}{fourcc}"))
}

fn has(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    // SAFETY: `a` is a valid, initialised address for the duration of the call.
    unsafe { AudioObjectHasProperty(id, NonNull::from(a)) }
}

fn settable(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    let mut out: u8 = 0;
    // SAFETY: valid pointers to locals.
    let st =
        unsafe { AudioObjectIsPropertySettable(id, NonNull::from(a), NonNull::from(&mut out)) };
    st == 0 && out != 0
}

fn get<T: Copy + Default>(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Result<T> {
    let mut v = T::default();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `v` is a valid `T` buffer of `size` bytes; T is a plain-old-data CoreAudio type.
    let st = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut v).cast(),
        )
    };
    if st != 0 {
        return Err(os_err("get", st));
    }
    Ok(v)
}

fn set<T: Copy>(id: AudioObjectID, a: &AudioObjectPropertyAddress, v: T) -> Result<()> {
    // SAFETY: `v` is a valid `T` of `size_of::<T>()` bytes; CoreAudio copies it.
    let st = unsafe {
        AudioObjectSetPropertyData(
            id,
            NonNull::from(a),
            0,
            std::ptr::null(),
            size_of::<T>() as u32,
            NonNull::from(&v).cast(),
        )
    };
    if st != 0 {
        return Err(os_err("set", st));
    }
    Ok(())
}

fn get_vec<T: Copy + Default>(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Result<Vec<T>> {
    let mut size: u32 = 0;
    // SAFETY: valid pointers to locals.
    let st = unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if st != 0 {
        return Err(os_err("size", st));
    }
    let n = size as usize / size_of::<T>();
    let mut v = vec![T::default(); n];
    if n == 0 {
        return Ok(v);
    }
    // SAFETY: `v` holds `n` initialised `T`s = `size` bytes (rounded down).
    let mut size = (n * size_of::<T>()) as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(v.as_mut_ptr()).unwrap().cast(),
        )
    };
    if st != 0 {
        return Err(os_err("get", st));
    }
    v.truncate(size as usize / size_of::<T>());
    Ok(v)
}

fn get_string(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Result<String> {
    let ptr: *const c_void = get::<usize>(id, a)? as *const c_void;
    let Some(nn) = NonNull::new(ptr.cast_mut().cast::<CFString>()) else {
        return Ok(String::new());
    };
    // SAFETY: name/UID properties return a +1 retained CFStringRef ("copy" semantics);
    // CFRetained releases it on drop.
    let s = unsafe { CFRetained::from_raw(nn) };
    Ok(s.to_string())
}

fn has_streams(id: AudioObjectID, scope: u32) -> bool {
    get_vec::<u32>(
        id,
        &addr(
            kAudioDevicePropertyStreams,
            scope,
            kAudioObjectPropertyElementMain,
        ),
    )
    .map(|v| !v.is_empty())
    .unwrap_or(false)
}

/// Elements carrying `selector` in `scope`: `[0]` if the main element has it, else the
/// per-channel elements 1..=8 that do.
fn elements(id: AudioObjectID, selector: u32, scope: u32) -> Vec<u32> {
    if has(id, &addr(selector, scope, kAudioObjectPropertyElementMain)) {
        return vec![kAudioObjectPropertyElementMain];
    }
    (1..=8)
        .filter(|&e| has(id, &addr(selector, scope, e)))
        .collect()
}

/// One HAL device as seen during enumeration (public for `yeti-probe` diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalDevice {
    pub id: AudioObjectID,
    pub name: String,
    pub uid: String,
    pub has_input: bool,
    pub has_output: bool,
    pub usb: bool,
}

/// All HAL audio devices.
pub fn list_devices() -> Result<Vec<HalDevice>> {
    let ids: Vec<AudioObjectID> = get_vec(
        SYSTEM_OBJECT,
        &addr(
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
    )?;
    Ok(ids
        .into_iter()
        .map(|id| {
            let g = |sel| {
                addr(
                    sel,
                    kAudioObjectPropertyScopeGlobal,
                    kAudioObjectPropertyElementMain,
                )
            };
            HalDevice {
                id,
                name: get_string(id, &g(kAudioObjectPropertyName)).unwrap_or_default(),
                uid: get_string(id, &g(kAudioDevicePropertyDeviceUID)).unwrap_or_default(),
                has_input: has_streams(id, kAudioObjectPropertyScopeInput),
                has_output: has_streams(id, kAudioObjectPropertyScopeOutput),
                usb: get::<u32>(id, &g(kAudioDevicePropertyTransportType)).ok()
                    == Some(kAudioDeviceTransportTypeUSB),
            }
        })
        .collect())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ids {
    input: Option<HalDevice>,
    output: Option<HalDevice>,
}

fn scan() -> Ids {
    let devs = list_devices().unwrap_or_default();
    let mut yetis: Vec<_> = devs
        .into_iter()
        .filter(|d| d.name.contains(YETI_NAME))
        .collect();
    // Prefer USB transport (a renamed aggregate/virtual device should never win).
    yetis.sort_by_key(|d| !d.usb);
    Ids {
        input: yetis.iter().find(|d| d.has_input).cloned(),
        output: yetis.iter().find(|d| d.has_output).cloned(),
    }
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

fn device_listen_addrs(input: bool) -> Vec<AudioObjectPropertyAddress> {
    if input {
        vec![
            addr(
                kAudioDevicePropertyVolumeScalar,
                kAudioObjectPropertyScopeInput,
                ELEMENT_WILDCARD,
            ),
            addr(
                kAudioDevicePropertyMute,
                kAudioObjectPropertyScopeInput,
                ELEMENT_WILDCARD,
            ),
            addr(
                kAudioDevicePropertyNominalSampleRate,
                kAudioObjectPropertyScopeGlobal,
                kAudioObjectPropertyElementMain,
            ),
        ]
    } else {
        vec![
            addr(
                kAudioDevicePropertyVolumeScalar,
                kAudioObjectPropertyScopeOutput,
                ELEMENT_WILDCARD,
            ),
            addr(
                kAudioDevicePropertyMute,
                kAudioObjectPropertyScopeOutput,
                ELEMENT_WILDCARD,
            ),
        ]
    }
}

fn devices_addr() -> AudioObjectPropertyAddress {
    addr(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    )
}

struct Inner {
    ids: Mutex<Ids>,
    cb: Mutex<Option<DeviceEventCallback>>,
}

unsafe extern "C-unwind" fn listener_proc(
    object: AudioObjectID,
    _n: u32,
    _addrs: NonNull<AudioObjectPropertyAddress>,
    data: *mut c_void,
) -> i32 {
    // SAFETY: `data` is `Arc::as_ptr(&inner)`; listeners are removed in `Drop` before the
    // last `Arc` is released.
    let inner = unsafe { &*(data as *const Inner) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner.on_notification(object == SYSTEM_OBJECT);
    }));
    0
}

impl Inner {
    /// Listener client data: the address of this `Inner` (inside its `Arc`).
    fn client_data(&self) -> *mut c_void {
        self as *const Inner as *mut c_void
    }

    /// Add (`add = true`) or remove the per-device property listeners for `ids`.
    fn bind(&self, ids: &Ids, add: bool) {
        for (dev, is_in) in [(&ids.input, true), (&ids.output, false)] {
            let Some(d) = dev else { continue };
            for a in device_listen_addrs(is_in) {
                // SAFETY: valid address; `listener_proc` matches the C signature; client data
                // outlives the registration (see `Drop`). Failures are non-fatal (a vanished
                // device drops its listeners itself).
                unsafe {
                    if add {
                        AudioObjectAddPropertyListener(
                            d.id,
                            NonNull::from(&a),
                            Some(listener_proc),
                            self.client_data(),
                        );
                    } else {
                        AudioObjectRemovePropertyListener(
                            d.id,
                            NonNull::from(&a),
                            Some(listener_proc),
                            self.client_data(),
                        );
                    }
                }
            }
        }
    }

    fn emit(&self, ev: DeviceEvent) {
        let cb = self.cb.lock().unwrap().clone();
        if let Some(cb) = cb {
            cb(ev);
        }
    }

    fn on_notification(&self, system: bool) {
        if system {
            let new = scan();
            let (was, old) = {
                let mut ids = self.ids.lock().unwrap();
                let old = ids.clone();
                if old == new {
                    return;
                }
                *ids = new.clone();
                (old.input.is_some(), old)
            };
            self.bind(&old, false);
            self.bind(&new, true);
            let now = new.input.is_some();
            if was && !now {
                self.emit(DeviceEvent::Disconnected);
            } else if now {
                let st = state_of(&new);
                self.emit(if was {
                    DeviceEvent::Changed(st)
                } else {
                    DeviceEvent::Connected(st)
                });
            }
        } else {
            let ids = self.ids.lock().unwrap().clone();
            if ids.input.is_some() {
                self.emit(DeviceEvent::Changed(state_of(&ids)));
            }
        }
    }
}

fn state_of(ids: &Ids) -> DeviceState {
    let Some(inp) = &ids.input else {
        return DeviceState {
            connected: false,
            name: YETI_NAME.to_string(),
            ..Default::default()
        };
    };
    let id = inp.id;
    let scope_in = kAudioObjectPropertyScopeInput;
    let gain_els = elements(id, kAudioDevicePropertyVolumeDecibels, scope_in);
    let gains: Vec<f32> = gain_els
        .iter()
        .filter_map(|&e| {
            get::<f32>(id, &addr(kAudioDevicePropertyVolumeDecibels, scope_in, e)).ok()
        })
        .collect();
    let input_gain_db = (!gains.is_empty()).then(|| gains.iter().sum::<f32>() / gains.len() as f32);
    let input_gain_range_db = gain_els.first().and_then(|&e| {
        get::<ValueRange>(
            id,
            &addr(kAudioDevicePropertyVolumeRangeDecibels, scope_in, e),
        )
        .ok()
        .map(|r| (r.min as f32, r.max as f32))
    });
    let input_muted = elements(id, kAudioDevicePropertyMute, scope_in)
        .first()
        .and_then(|&e| get::<u32>(id, &addr(kAudioDevicePropertyMute, scope_in, e)).ok())
        .map(|m| m != 0);
    let g = |sel| {
        addr(
            sel,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        )
    };
    let sample_rate = get::<f64>(id, &g(kAudioDevicePropertyNominalSampleRate))
        .ok()
        .map(|r| r.round() as u32);
    let available_sample_rates = available_rates(id);
    let sample_rate_settable = settable(id, &g(kAudioDevicePropertyNominalSampleRate));

    let (mut headphone_volume, mut headphone_muted) = (None, None);
    if let Some(out) = &ids.output {
        let so = kAudioObjectPropertyScopeOutput;
        let vols: Vec<f32> = elements(out.id, kAudioDevicePropertyVolumeScalar, so)
            .iter()
            .filter_map(|&e| {
                get::<f32>(out.id, &addr(kAudioDevicePropertyVolumeScalar, so, e)).ok()
            })
            .collect();
        headphone_volume = (!vols.is_empty()).then(|| vols.iter().sum::<f32>() / vols.len() as f32);
        headphone_muted = elements(out.id, kAudioDevicePropertyMute, so)
            .first()
            .and_then(|&e| get::<u32>(out.id, &addr(kAudioDevicePropertyMute, so, e)).ok())
            .map(|m| m != 0);
    }
    DeviceState {
        connected: true,
        name: inp.name.clone(),
        uid: inp.uid.clone(),
        input_gain_db,
        input_gain_range_db,
        input_muted,
        headphone_volume,
        headphone_muted,
        sample_rate,
        available_sample_rates,
        sample_rate_settable,
    }
}

fn available_rates(id: AudioObjectID) -> Vec<u32> {
    let ranges: Vec<ValueRange> = get_vec(
        id,
        &addr(
            kAudioDevicePropertyAvailableNominalSampleRates,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        ),
    )
    .unwrap_or_default();
    let mut v: Vec<u32> = Vec::new();
    for r in ranges {
        for std_rate in [
            8_000u32, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400,
            192_000,
        ] {
            if (r.min..=r.max).contains(&(std_rate as f64)) && !v.contains(&std_rate) {
                v.push(std_rate);
            }
        }
    }
    v.sort_unstable();
    v
}

// ---------------------------------------------------------------------------
// Public type
// ---------------------------------------------------------------------------

/// CoreAudio-backed Yeti. Tracks hot-plug: the HAL device ids are re-resolved whenever the
/// system device list changes, so one instance stays valid across unplug/replug.
pub struct CoreAudioYeti {
    inner: Arc<Inner>,
}

impl CoreAudioYeti {
    /// Locate the Yeti's CoreAudio input/output devices; `DeviceNotFound` if absent.
    pub fn open() -> Result<Self> {
        let me = Self::watch()?;
        if me.inner.ids.lock().unwrap().input.is_none() {
            return Err(YetiError::DeviceNotFound);
        }
        Ok(me)
    }

    /// Like [`open`](Self::open) but succeeds while the Yeti is unplugged
    /// (`state().connected == false`); a `Connected` event fires when it appears.
    pub fn watch() -> Result<Self> {
        let inner = Arc::new(Inner {
            ids: Mutex::new(scan()),
            cb: Mutex::new(None),
        });
        let a = devices_addr();
        // SAFETY: valid address and callback; unregistered in Drop.
        let st = unsafe {
            AudioObjectAddPropertyListener(
                SYSTEM_OBJECT,
                NonNull::from(&a),
                Some(listener_proc),
                inner.client_data(),
            )
        };
        if st != 0 {
            return Err(os_err("add device-list listener", st));
        }
        let ids = inner.ids.lock().unwrap().clone();
        inner.bind(&ids, true);
        Ok(Self { inner })
    }

    /// HAL object id of the Yeti's input device (for diagnostics).
    pub fn input_device_id(&self) -> Option<AudioObjectID> {
        self.inner.ids.lock().unwrap().input.as_ref().map(|d| d.id)
    }

    /// HAL object id of the Yeti's output (headphone) device.
    pub fn output_device_id(&self) -> Option<AudioObjectID> {
        self.inner.ids.lock().unwrap().output.as_ref().map(|d| d.id)
    }

    fn input_id(&self) -> Result<AudioObjectID> {
        self.input_device_id().ok_or(YetiError::DeviceDisconnected)
    }

    fn output_id(&self) -> Result<AudioObjectID> {
        if self.input_device_id().is_none() {
            return Err(YetiError::DeviceDisconnected);
        }
        self.output_device_id()
            .ok_or(YetiError::Unsupported("Yeti headphone output not present"))
    }

    fn set_on_elements<T: Copy>(
        &self,
        id: AudioObjectID,
        selector: u32,
        scope: u32,
        v: T,
        what: &'static str,
    ) -> Result<()> {
        let els = elements(id, selector, scope);
        if els.is_empty() {
            return Err(YetiError::Unsupported(what));
        }
        for e in els {
            let a = addr(selector, scope, e);
            if !settable(id, &a) {
                return Err(YetiError::Unsupported(what));
            }
            set(id, &a, v)?;
        }
        Ok(())
    }
}

impl Drop for CoreAudioYeti {
    fn drop(&mut self) {
        let a = devices_addr();
        // SAFETY: same address/proc/data as registered in `watch`.
        unsafe {
            AudioObjectRemovePropertyListener(
                SYSTEM_OBJECT,
                NonNull::from(&a),
                Some(listener_proc),
                self.inner.client_data(),
            );
        }
        let ids = self.inner.ids.lock().unwrap().clone();
        self.inner.bind(&ids, false);
    }
}

impl DeviceControl for CoreAudioYeti {
    fn state(&self) -> Result<DeviceState> {
        let ids = self.inner.ids.lock().unwrap().clone();
        Ok(state_of(&ids))
    }

    fn set_input_gain_db(&self, db: f32) -> Result<()> {
        let id = self.input_id()?;
        let st = self.state()?;
        let (lo, hi) = st
            .input_gain_range_db
            .ok_or(YetiError::Unsupported("input gain"))?;
        if !db.is_finite() || db < lo - 1e-3 || db > hi + 1e-3 {
            return Err(YetiError::OutOfRange(format!("{db} dB not in {lo}..={hi}")));
        }
        self.set_on_elements(
            id,
            kAudioDevicePropertyVolumeDecibels,
            kAudioObjectPropertyScopeInput,
            db.clamp(lo, hi),
            "input gain",
        )
    }

    fn set_input_muted(&self, muted: bool) -> Result<()> {
        let id = self.input_id()?;
        self.set_on_elements(
            id,
            kAudioDevicePropertyMute,
            kAudioObjectPropertyScopeInput,
            muted as u32,
            "input mute",
        )
    }

    fn set_headphone_volume(&self, volume: f32) -> Result<()> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(YetiError::OutOfRange(format!("volume {volume}")));
        }
        let id = self.output_id()?;
        self.set_on_elements(
            id,
            kAudioDevicePropertyVolumeScalar,
            kAudioObjectPropertyScopeOutput,
            volume,
            "headphone volume",
        )
    }

    fn set_headphone_muted(&self, muted: bool) -> Result<()> {
        let id = self.output_id()?;
        self.set_on_elements(
            id,
            kAudioDevicePropertyMute,
            kAudioObjectPropertyScopeOutput,
            muted as u32,
            "headphone mute",
        )
    }

    fn set_sample_rate(&self, hz: u32) -> Result<()> {
        let id = self.input_id()?;
        if !available_rates(id).contains(&hz) {
            return Err(YetiError::OutOfRange(format!("{hz} Hz")));
        }
        let a = addr(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
            kAudioObjectPropertyElementMain,
        );
        if !settable(id, &a) {
            return Err(YetiError::Unsupported("sample rate"));
        }
        set(id, &a, hz as f64)?;
        // Keep the headphone side on the same clock rate when it supports it (best effort).
        if let Some(out) = self.output_device_id()
            && available_rates(out).contains(&hz)
            && settable(out, &a)
        {
            let _ = set(out, &a, hz as f64);
        }
        Ok(())
    }

    fn set_event_callback(&self, cb: Option<DeviceEventCallback>) {
        *self.inner.cb.lock().unwrap() = cb;
    }
}
