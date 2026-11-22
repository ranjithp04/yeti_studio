//! Windows backend: MMDevice API + `IAudioEndpointVolume` (`windows` crate). Phase 6.
//!
//! Owner: **Windows engineer**.
//!
//! - The Yeti is found by enumerating **active** capture/render endpoints and matching
//!   [`crate::YETI_NAME`] in the interface or friendly name ([`endpoint_names::is_yeti_endpoint`]).
//! - Input gain (dB) / input mute: `IAudioEndpointVolume` on the Yeti **capture** endpoint.
//! - Headphone volume (scalar 0..=1) / mute: `IAudioEndpointVolume` on the Yeti **render** endpoint.
//! - Sample rate: **read-only** (no public API to change a shared-mode device rate). Read from
//!   `IAudioClient::GetMixFormat`; `set_sample_rate` returns `Unsupported`.
//! - Hot-plug and external changes (Yeti mute button, Sound control panel): `IMMNotificationClient`
//!   and `IAudioEndpointVolumeCallback`, forwarded to a watcher thread that queries the new state
//!   and calls the user callback (never from the COM notification thread itself, which must not
//!   block or call back into MMDevice).
//! - VB-CABLE detection: [`find_vb_cable_input`] / [`vb_cable_input`] ("CABLE Input" is the
//!   engine's output target; absent → [`YetiError::VirtualMicMissing`] with
//!   [`endpoint_names::VB_CABLE_MISSING_MESSAGE`]).
//!
//! COM: no interface pointer is stored across calls. Every call initialises COM for the calling
//! thread (MTA, or reuses an existing STA such as the UI thread), re-resolves the endpoint by its
//! stable id and releases everything before returning. That keeps [`WasapiYeti`] `Send + Sync`
//! and makes unplug/replug safe. Cost: a few ms per call — fine for UI actions, never call from
//! an audio callback (same contract as [`DeviceControl`]).
//!
//! Note: this module shadows the `windows` crate name; refer to the crate as `::windows`.

use crate::endpoint_names::{self, VB_CABLE_MISSING_MESSAGE};
use crate::{DeviceControl, DeviceEventCallback};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use yeti_types::{DeviceEvent, DeviceState, Result, YetiError};

use ::windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName,
};
use ::windows::Win32::Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE};
use ::windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use ::windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IAudioClient, IMMDevice, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, eCapture, eCommunications, eConsole, eRender,
};
use ::windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use ::windows::core::{HSTRING, PCWSTR, implement};

/// COM interfaces the backend is built on.
pub type DeviceEnumerator = ::windows::Win32::Media::Audio::IMMDeviceEnumerator;
pub type EndpointVolume = ::windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;

// ---------------------------------------------------------------------------
// Public endpoint helpers (used by yeti-engine's Windows routing)
// ---------------------------------------------------------------------------

/// Endpoint direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Recording endpoint (microphones, "CABLE Output").
    Capture,
    /// Playback endpoint (speakers, Yeti headphone jack, "CABLE Input").
    Render,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Capture => eCapture,
            Flow::Render => eRender,
        }
    }
}

/// Default-device role (Windows keeps separate defaults per role).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Games, media, system sounds.
    Console,
    /// Voice chat / meeting apps (Teams, Zoom, Discord use this one by default).
    Communications,
}

/// An active MMDevice endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    /// Stable MMDevice endpoint id (`IMMDevice::GetId`). Equal to cpal's WASAPI
    /// `DeviceId::id()`, so the engine can open exactly this endpoint with cpal.
    pub id: String,
    /// `PKEY_Device_FriendlyName`, e.g. "Microphone (Yeti Stereo Microphone)".
    pub friendly_name: String,
    /// `PKEY_DeviceInterface_FriendlyName`, e.g. "Yeti Stereo Microphone" (may be empty).
    pub interface_name: String,
    pub flow: Flow,
}

/// All **active** endpoints of one direction.
pub fn list_endpoints(flow: Flow) -> Result<Vec<EndpointInfo>> {
    let _com = ComGuard::new()?;
    let en = enumerator()?;
    Ok(active_devices(&en, flow)?
        .into_iter()
        .map(|(_, info)| info)
        .collect())
}

/// VB-CABLE's "CABLE Input" render endpoint, if installed and enabled.
pub fn find_vb_cable_input() -> Result<Option<EndpointInfo>> {
    Ok(list_endpoints(Flow::Render)?
        .into_iter()
        .find(|e| endpoint_names::is_vb_cable_input(&e.friendly_name, &e.interface_name)))
}

/// Like [`find_vb_cable_input`] but absent → [`YetiError::VirtualMicMissing`] carrying
/// [`VB_CABLE_MISSING_MESSAGE`] (the UI's "Install VB-CABLE" state).
pub fn vb_cable_input() -> Result<EndpointInfo> {
    find_vb_cable_input()?
        .ok_or_else(|| YetiError::VirtualMicMissing(VB_CABLE_MISSING_MESSAGE.to_string()))
}

/// Current default render endpoint for `role` (the AEC loopback reference source).
/// `Ok(None)` when no playback device exists.
pub fn default_render_endpoint(role: Role) -> Result<Option<EndpointInfo>> {
    let _com = ComGuard::new()?;
    let en = enumerator()?;
    let role: ERole = match role {
        Role::Console => eConsole,
        Role::Communications => eCommunications,
    };
    match unsafe { en.GetDefaultAudioEndpoint(eRender, role) } {
        Ok(dev) => Ok(Some(endpoint_info(&dev, Flow::Render)?)),
        // E_NOTFOUND (HRESULT_FROM_WIN32(ERROR_NOT_FOUND)) when there is no render device.
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// WasapiYeti
// ---------------------------------------------------------------------------

/// Last-known Yeti endpoints (ids are stable across unplug/replug on the same port).
#[derive(Debug, Default)]
struct Ids {
    capture: Option<EndpointInfo>,
    render: Option<EndpointInfo>,
}

struct Shared {
    ids: Mutex<Ids>,
    cb: Mutex<Option<DeviceEventCallback>>,
}

struct Watcher {
    tx: Sender<Msg>,
    handle: JoinHandle<()>,
}

enum Msg {
    /// Some endpoint was added / removed / changed state.
    Topology,
    /// Volume or mute changed on a Yeti endpoint.
    Volume,
    Stop,
}

/// MMDevice/WASAPI-backed Yeti.
pub struct WasapiYeti {
    shared: Arc<Shared>,
    watcher: Mutex<Option<Watcher>>,
}

impl WasapiYeti {
    /// Locate the Yeti's capture (and, if present, render) endpoint.
    /// [`YetiError::DeviceNotFound`] if no active Yeti capture endpoint exists.
    pub fn open() -> Result<Self> {
        let shared = Arc::new(Shared {
            ids: Mutex::new(Ids::default()),
            cb: Mutex::new(None),
        });
        {
            let _com = ComGuard::new()?;
            let en = enumerator()?;
            let r = resolve(&en, &shared)?;
            if r.capture.is_none() {
                return Err(YetiError::DeviceNotFound);
            }
        }
        Ok(Self {
            shared,
            watcher: Mutex::new(None),
        })
    }

    /// Last-resolved Yeti endpoints `(capture, render)`. The engine opens these with cpal by id.
    pub fn endpoints(&self) -> (Option<EndpointInfo>, Option<EndpointInfo>) {
        let ids = lock(&self.shared.ids);
        (ids.capture.clone(), ids.render.clone())
    }

    fn with_volume<T>(
        &self,
        flow: Flow,
        f: impl FnOnce(&EndpointVolume) -> ::windows::core::Result<T>,
    ) -> Result<T> {
        let _com = ComGuard::new()?;
        let en = enumerator()?;
        let r = resolve(&en, &self.shared)?;
        let dev = match flow {
            Flow::Capture => r.capture.ok_or(YetiError::DeviceDisconnected)?,
            Flow::Render => r.render.ok_or(YetiError::Unsupported(
                "Yeti headphone (render) endpoint not found or disabled",
            ))?,
        };
        let vol: EndpointVolume = unsafe { dev.Activate(CLSCTX_ALL, None) }
            .map_err(|e| map_err("activate IAudioEndpointVolume", e))?;
        f(&vol).map_err(|e| map_err("IAudioEndpointVolume", e))
    }

    fn stop_watcher(&self) {
        let w = lock(&self.watcher).take();
        if let Some(w) = w {
            let _ = w.tx.send(Msg::Stop);
            let _ = w.handle.join();
        }
    }
}

impl Drop for WasapiYeti {
    fn drop(&mut self) {
        self.stop_watcher();
    }
}

impl DeviceControl for WasapiYeti {
    fn state(&self) -> Result<DeviceState> {
        query_state(&self.shared)
    }

    fn set_input_gain_db(&self, db: f32) -> Result<()> {
        self.with_volume(Flow::Capture, |v| unsafe {
            let (lo, hi) = volume_range(v)?;
            if !db.is_finite() || db < lo || db > hi {
                return Ok(Err(YetiError::OutOfRange(format!(
                    "{db} dB not in {lo}..={hi}"
                ))));
            }
            v.SetMasterVolumeLevel(db, std::ptr::null()).map(Ok)
        })?
    }

    fn set_input_muted(&self, muted: bool) -> Result<()> {
        self.with_volume(Flow::Capture, |v| unsafe {
            v.SetMute(muted, std::ptr::null())
        })
    }

    fn set_headphone_volume(&self, volume: f32) -> Result<()> {
        if !(0.0..=1.0).contains(&volume) {
            return Err(YetiError::OutOfRange(format!("volume {volume}")));
        }
        self.with_volume(Flow::Render, |v| unsafe {
            v.SetMasterVolumeLevelScalar(volume, std::ptr::null())
        })
    }

    fn set_headphone_muted(&self, muted: bool) -> Result<()> {
        self.with_volume(Flow::Render, |v| unsafe {
            v.SetMute(muted, std::ptr::null())
        })
    }

    fn set_sample_rate(&self, _hz: u32) -> Result<()> {
        Err(YetiError::Unsupported(
            "sample rate is read-only on Windows (change it in Sound settings > Properties > Advanced)",
        ))
    }

    fn set_event_callback(&self, cb: Option<DeviceEventCallback>) {
        let enable = cb.is_some();
        *lock(&self.shared.cb) = cb;
        if !enable {
            self.stop_watcher();
            return;
        }
        let mut w = lock(&self.watcher);
        if w.is_none() {
            let (tx, rx) = mpsc::channel();
            let shared = self.shared.clone();
            let tx2 = tx.clone();
            match std::thread::Builder::new()
                .name("yeti-wasapi-watch".into())
                .spawn(move || watcher_main(shared, tx2, rx))
            {
                Ok(handle) => *w = Some(Watcher { tx, handle }),
                Err(e) => log::error!("cannot start Yeti device watcher: {e}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Watcher thread (hot-plug + external volume/mute changes)
// ---------------------------------------------------------------------------

#[implement(IMMNotificationClient)]
struct NotificationClient {
    tx: Sender<Msg>,
}

impl IMMNotificationClient_Impl for NotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &PCWSTR,
        _state: DEVICE_STATE,
    ) -> ::windows::core::Result<()> {
        let _ = self.tx.send(Msg::Topology);
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> ::windows::core::Result<()> {
        let _ = self.tx.send(Msg::Topology);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> ::windows::core::Result<()> {
        let _ = self.tx.send(Msg::Topology);
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _id: &PCWSTR,
    ) -> ::windows::core::Result<()> {
        Ok(())
    }
    fn OnPropertyValueChanged(
        &self,
        _id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> ::windows::core::Result<()> {
        Ok(())
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    tx: Sender<Msg>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> ::windows::core::Result<()> {
        let _ = self.tx.send(Msg::Volume);
        Ok(())
    }
}

/// Coalesce bursts (one replug fires several notifications) before querying the OS.
const DEBOUNCE: Duration = Duration::from_millis(150);

fn watcher_main(shared: Arc<Shared>, tx: Sender<Msg>, rx: Receiver<Msg>) {
    let _com = match ComGuard::new() {
        Ok(g) => g,
        Err(e) => return log::error!("Yeti watcher: {e}"),
    };
    let en = match enumerator() {
        Ok(en) => en,
        Err(e) => return log::error!("Yeti watcher: {e}"),
    };
    let client: IMMNotificationClient = NotificationClient { tx: tx.clone() }.into();
    if let Err(e) = unsafe { en.RegisterEndpointNotificationCallback(&client) } {
        log::error!("Yeti watcher: RegisterEndpointNotificationCallback: {e}");
    }
    let vol_cb: IAudioEndpointVolumeCallback = VolumeCallback { tx }.into();
    let mut registered: Vec<EndpointVolume> = Vec::new();
    register_volume_callbacks(&en, &shared, &vol_cb, &mut registered);
    let mut connected = query_state(&shared).map(|s| s.connected).unwrap_or(false);

    'outer: while let Ok(first) = rx.recv() {
        let (mut topology, mut volume) = (false, false);
        let mut next = Some(first);
        while let Some(m) = next {
            match m {
                Msg::Stop => break 'outer,
                Msg::Topology => topology = true,
                Msg::Volume => volume = true,
            }
            next = match rx.recv_timeout(DEBOUNCE) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break 'outer,
            };
        }
        if topology {
            unregister_volume_callbacks(&vol_cb, &mut registered);
            register_volume_callbacks(&en, &shared, &vol_cb, &mut registered);
        }
        let state = match query_state(&shared) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Yeti watcher: state query failed: {e}");
                continue;
            }
        };
        let event = match (connected, state.connected) {
            (false, true) => Some(DeviceEvent::Connected(state.clone())),
            (true, false) => Some(DeviceEvent::Disconnected),
            (true, true) if volume => Some(DeviceEvent::Changed(state.clone())),
            _ => None,
        };
        connected = state.connected;
        if let Some(ev) = event {
            let cb = lock(&shared.cb).clone();
            if let Some(cb) = cb {
                cb(ev);
            }
        }
    }

    unregister_volume_callbacks(&vol_cb, &mut registered);
    let _ = unsafe { en.UnregisterEndpointNotificationCallback(&client) };
}

fn register_volume_callbacks(
    en: &DeviceEnumerator,
    shared: &Shared,
    cb: &IAudioEndpointVolumeCallback,
    out: &mut Vec<EndpointVolume>,
) {
    let Ok(r) = resolve(en, shared) else { return };
    for dev in [r.capture, r.render].into_iter().flatten() {
        let vol: ::windows::core::Result<EndpointVolume> =
            unsafe { dev.Activate(CLSCTX_ALL, None) };
        match vol {
            Ok(v) => match unsafe { v.RegisterControlChangeNotify(cb) } {
                Ok(()) => out.push(v),
                Err(e) => log::warn!("Yeti watcher: RegisterControlChangeNotify: {e}"),
            },
            Err(e) => log::warn!("Yeti watcher: activate IAudioEndpointVolume: {e}"),
        }
    }
}

fn unregister_volume_callbacks(cb: &IAudioEndpointVolumeCallback, regs: &mut Vec<EndpointVolume>) {
    for v in regs.drain(..) {
        let _ = unsafe { v.UnregisterControlChangeNotify(cb) };
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Per-call COM initialisation. Must be declared **before** any COM object in a scope so it is
/// dropped (and `CoUninitialize` runs) after them.
struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    fn new() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            // S_OK or S_FALSE (already initialised as MTA): balance with CoUninitialize.
            Ok(Self { uninit: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // Thread is already STA (e.g. the UI thread): COM is usable; do not uninitialise.
            Ok(Self { uninit: false })
        } else {
            Err(YetiError::Backend(format!("CoInitializeEx: {hr:?}")))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { CoUninitialize() };
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn map_err(ctx: &str, e: ::windows::core::Error) -> YetiError {
    if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        YetiError::DeviceDisconnected
    } else {
        YetiError::Backend(format!("{ctx}: {e}"))
    }
}

fn enumerator() -> Result<DeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| map_err("create IMMDeviceEnumerator", e))
}

fn read_string_prop(
    store: &::windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore,
    key: &PROPERTYKEY,
) -> String {
    match unsafe { store.GetValue(key) } {
        Ok(v) => v.to_string(),
        Err(_) => String::new(),
    }
}

fn endpoint_info(dev: &IMMDevice, flow: Flow) -> Result<EndpointInfo> {
    let id = unsafe {
        let p = dev.GetId().map_err(|e| map_err("IMMDevice::GetId", e))?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map_err(|e| YetiError::Backend(format!("endpoint id: {e}")))?
    };
    let (friendly_name, interface_name) = match unsafe { dev.OpenPropertyStore(STGM_READ) } {
        Ok(store) => (
            read_string_prop(&store, &PKEY_Device_FriendlyName),
            read_string_prop(&store, &PKEY_DeviceInterface_FriendlyName),
        ),
        Err(_) => (String::new(), String::new()),
    };
    Ok(EndpointInfo {
        id,
        friendly_name,
        interface_name,
        flow,
    })
}

fn active_devices(en: &DeviceEnumerator, flow: Flow) -> Result<Vec<(IMMDevice, EndpointInfo)>> {
    let col = unsafe { en.EnumAudioEndpoints(flow.data_flow(), DEVICE_STATE_ACTIVE) }
        .map_err(|e| map_err("EnumAudioEndpoints", e))?;
    let n = unsafe { col.GetCount() }.map_err(|e| map_err("IMMDeviceCollection::GetCount", e))?;
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Ok(dev) = (unsafe { col.Item(i) }) else {
            continue;
        };
        if let Ok(info) = endpoint_info(&dev, flow) {
            out.push((dev, info));
        }
    }
    Ok(out)
}

/// The endpoint with this id, if it still exists and is active.
fn active_by_id(en: &DeviceEnumerator, id: &str) -> Option<IMMDevice> {
    let h = HSTRING::from(id);
    let dev = unsafe { en.GetDevice(PCWSTR(h.as_ptr())) }.ok()?;
    let state = unsafe { dev.GetState() }.ok()?;
    (state == DEVICE_STATE_ACTIVE).then_some(dev)
}

struct Resolved {
    capture: Option<IMMDevice>,
    render: Option<IMMDevice>,
}

/// Re-resolve the Yeti endpoints: reuse the stored ids while they are active, otherwise rescan.
/// Stored infos are kept when the device disappears (for name/uid in the disconnected state).
fn resolve(en: &DeviceEnumerator, shared: &Shared) -> Result<Resolved> {
    let mut ids = lock(&shared.ids);
    let pick = |slot: &mut Option<EndpointInfo>, flow: Flow| -> Result<Option<IMMDevice>> {
        if let Some(dev) = slot.as_ref().and_then(|i| active_by_id(en, &i.id)) {
            return Ok(Some(dev));
        }
        let found = active_devices(en, flow)?
            .into_iter()
            .find(|(_, i)| endpoint_names::is_yeti_endpoint(&i.friendly_name, &i.interface_name));
        Ok(found.map(|(dev, info)| {
            *slot = Some(info);
            dev
        }))
    };
    let capture = pick(&mut ids.capture, Flow::Capture)?;
    let render = pick(&mut ids.render, Flow::Render)?;
    Ok(Resolved { capture, render })
}

fn volume_range(v: &EndpointVolume) -> ::windows::core::Result<(f32, f32)> {
    let (mut lo, mut hi, mut step) = (0f32, 0f32, 0f32);
    unsafe { v.GetVolumeRange(&mut lo, &mut hi, &mut step)? };
    Ok((lo, hi))
}

/// Mix-format sample rate of the endpoint (the shared-mode rate set in Sound settings).
fn mix_rate(dev: &IMMDevice) -> Option<u32> {
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).ok()?;
        let fmt = client.GetMixFormat().ok()?;
        if fmt.is_null() {
            return None;
        }
        // WAVEFORMATEX is packed(1): read the field unaligned.
        let rate = std::ptr::addr_of!((*fmt).nSamplesPerSec).read_unaligned();
        CoTaskMemFree(Some(fmt as *const _));
        Some(rate)
    }
}

fn query_state(shared: &Shared) -> Result<DeviceState> {
    let _com = ComGuard::new()?;
    let en = enumerator()?;
    let r = resolve(&en, shared)?;
    let (cap_info, render_info) = {
        let ids = lock(&shared.ids);
        (ids.capture.clone(), ids.render.clone())
    };
    let display_name = |i: &Option<EndpointInfo>| {
        i.as_ref()
            .map(|i| {
                if i.interface_name.is_empty() {
                    i.friendly_name.clone()
                } else {
                    i.interface_name.clone()
                }
            })
            .unwrap_or_else(|| crate::YETI_NAME.to_string())
    };
    let mut s = DeviceState {
        connected: false,
        name: display_name(&cap_info),
        uid: cap_info.as_ref().map(|i| i.id.clone()).unwrap_or_default(),
        sample_rate_settable: false,
        ..Default::default()
    };
    let Some(cap) = r.capture else {
        return Ok(s);
    };
    s.connected = true;
    let vol: EndpointVolume = unsafe { cap.Activate(CLSCTX_ALL, None) }
        .map_err(|e| map_err("activate IAudioEndpointVolume (capture)", e))?;
    unsafe {
        s.input_gain_db = vol.GetMasterVolumeLevel().ok();
        s.input_gain_range_db = volume_range(&vol).ok();
        s.input_muted = vol.GetMute().ok().map(|b| b.as_bool());
    }
    s.sample_rate = mix_rate(&cap);
    s.available_sample_rates = s.sample_rate.into_iter().collect();
    if render_info.is_some()
        && let Some(r) = r.render
        && let Ok(hv) = unsafe { r.Activate::<EndpointVolume>(CLSCTX_ALL, None) }
    {
        unsafe {
            s.headphone_volume = hv.GetMasterVolumeLevelScalar().ok();
            s.headphone_muted = hv.GetMute().ok().map(|b| b.as_bool());
        }
    }
    Ok(s)
}
