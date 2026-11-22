//! Windows routing (Phase 6). `cfg(windows)` only.
//!
//! Owner: **Windows engineer**. The real engine (`real.rs`, Device & engine engineer) calls these
//! on Windows instead of name-matching cpal devices itself:
//!
//! - [`virtual_mic_output`] — the virtual-mic target: VB-CABLE's **"CABLE Input"** render endpoint
//!   (chat apps then select "CABLE Output" as their microphone). Absent →
//!   [`YetiError::VirtualMicMissing`] carrying
//!   [`VB_CABLE_MISSING_MESSAGE`](yeti_device::endpoint_names::VB_CABLE_MISSING_MESSAGE); the
//!   engine should return it from `start()` and emit `EngineEvent::Warning` so the UI shows its
//!   "Install VB-CABLE" state. [`virtual_mic_installed`] is a cheap check for the UI/first run.
//! - [`yeti_capture_device`] / [`yeti_headphone_device`] — the Yeti endpoints, resolved through
//!   the MMDevice API (`yeti_device::windows`) and opened in cpal **by endpoint id**.
//! - [`start_loopback_reference`] — WASAPI loopback of a playback endpoint via cpal (cpal turns an
//!   input stream on a render device into `AUDCLNT_STREAMFLAGS_LOOPBACK`): the AEC far-end
//!   reference for the Meeting preset. Default source = the **communications** default playback
//!   device (what Teams/Zoom/Discord play the other side through).
//!
//! Loopback caveats (WASAPI behaviour, not verifiable on this Mac):
//! - Loopback captures *everything* rendered to that endpoint, **including our own "hear myself"
//!   monitor** if it plays to the same endpoint; AEC would then cancel the user's own voice.
//!   Check [`reference_includes_monitor`] and warn / disable the monitor while AEC is on.
//!   (Per-process exclusion needs `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, not in cpal.)
//! - When nothing is playing, a loopback stream delivers **no packets** (not zeros). The engine
//!   must treat a missing reference as silence, not count it as a reference underrun.
//! - The stream runs at the endpoint's mix format (rate/channels from Sound settings); the engine
//!   resamples to 48 kHz and downmixes, like any other capture.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use yeti_device::endpoint_names;
use yeti_device::windows::{self as wdev, EndpointInfo, Flow};
use yeti_types::{Result, YetiError};

pub use yeti_device::windows::Role;

/// Which playback endpoint feeds the AEC reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackSource {
    /// Default playback device for a role. Meeting preset: [`Role::Communications`].
    Default(Role),
    /// A specific render endpoint by MMDevice id ([`EndpointInfo::id`]).
    Endpoint(String),
}

impl Default for LoopbackSource {
    fn default() -> Self {
        LoopbackSource::Default(Role::Communications)
    }
}

/// A running loopback capture. Dropping it stops the stream.
pub struct LoopbackReference {
    pub stream: cpal::Stream,
    /// Stream format (the endpoint's mix format): interleaved `channels`, `sample_rate` Hz.
    pub config: StreamConfig,
    /// The playback endpoint being captured.
    pub endpoint: EndpointInfo,
}

fn cpal_err(ctx: &str, e: impl std::fmt::Display) -> YetiError {
    YetiError::Backend(format!("{ctx}: {e}"))
}

/// The WASAPI host (cpal's default on Windows).
pub fn host() -> cpal::Host {
    cpal::default_host()
}

/// Open an MMDevice endpoint in cpal, matching by endpoint id (cpal's WASAPI `DeviceId::id()`
/// is the MMDevice id), falling back to the friendly name.
pub fn cpal_device(host: &cpal::Host, ep: &EndpointInfo) -> Result<cpal::Device> {
    let devices: Vec<cpal::Device> = match ep.flow {
        Flow::Capture => host
            .input_devices()
            .map_err(|e| cpal_err("enumerate input devices", e))?
            .collect(),
        Flow::Render => host
            .output_devices()
            .map_err(|e| cpal_err("enumerate output devices", e))?
            .collect(),
    };
    if let Some(d) = devices
        .iter()
        .find(|d| d.id().is_ok_and(|id| id.id() == ep.id))
    {
        return Ok(d.clone());
    }
    devices
        .into_iter()
        .find(|d| {
            d.description()
                .is_ok_and(|desc| desc.name() == ep.friendly_name)
        })
        .ok_or_else(|| {
            YetiError::Backend(format!(
                "cpal has no WASAPI device for endpoint \"{}\"",
                ep.friendly_name
            ))
        })
}

/// `true` if VB-CABLE's "CABLE Input" is installed and enabled.
pub fn virtual_mic_installed() -> bool {
    matches!(wdev::find_vb_cable_input(), Ok(Some(_)))
}

/// Resolve the virtual-mic output device.
///
/// - `requested = None` (default): VB-CABLE "CABLE Input"; absent → `VirtualMicMissing`.
/// - `requested = Some(name)`: first active render endpoint whose friendly name matches
///   ([`endpoint_names::name_selects`]); not found → `VirtualMicMissing` naming it.
pub fn virtual_mic_output(
    host: &cpal::Host,
    requested: Option<&str>,
) -> Result<(cpal::Device, EndpointInfo)> {
    let ep = match requested {
        None => wdev::vb_cable_input()?,
        Some(name) => wdev::list_endpoints(Flow::Render)?
            .into_iter()
            .find(|e| endpoint_names::name_selects(name, &e.friendly_name))
            .ok_or_else(|| {
                YetiError::VirtualMicMissing(format!(
                    "output device \"{name}\" not found. {}",
                    endpoint_names::VB_CABLE_MISSING_MESSAGE
                ))
            })?,
    };
    Ok((cpal_device(host, &ep)?, ep))
}

fn find_yeti(flow: Flow) -> Result<Option<EndpointInfo>> {
    Ok(wdev::list_endpoints(flow)?
        .into_iter()
        .find(|e| endpoint_names::is_yeti_endpoint(&e.friendly_name, &e.interface_name)))
}

/// The Yeti's capture endpoint as a cpal device. Absent → `DeviceNotFound`.
pub fn yeti_capture_device(host: &cpal::Host) -> Result<(cpal::Device, EndpointInfo)> {
    let ep = find_yeti(Flow::Capture)?.ok_or(YetiError::DeviceNotFound)?;
    Ok((cpal_device(host, &ep)?, ep))
}

/// The Yeti's headphone-jack render endpoint (default "hear myself" target), if present.
pub fn yeti_headphone_device(host: &cpal::Host) -> Result<Option<(cpal::Device, EndpointInfo)>> {
    match find_yeti(Flow::Render)? {
        Some(ep) => Ok(Some((cpal_device(host, &ep)?, ep))),
        None => Ok(None),
    }
}

/// `true` if the loopback reference captures the endpoint the monitor plays to, i.e. the
/// "hear myself" signal would leak into the AEC reference.
pub fn reference_includes_monitor(
    reference: &EndpointInfo,
    monitor_endpoint_id: Option<&str>,
) -> bool {
    monitor_endpoint_id.is_some_and(|m| m == reference.id)
}

/// Max samples converted per step for non-f32 mix formats (stack buffer, no allocation).
const CONVERT_CHUNK: usize = 2048;

/// Start WASAPI loopback capture of a playback endpoint (AEC far-end reference).
///
/// `on_data(interleaved, channels)` runs on cpal's **real-time** callback thread: it must only
/// push into a lock-free ring (no alloc/lock/log), per the engine's RT rules. Samples are
/// delivered as `f32` at the endpoint's mix rate; non-f32 mix formats are converted in a fixed
/// stack buffer.
pub fn start_loopback_reference<D, E>(
    host: &cpal::Host,
    source: &LoopbackSource,
    on_data: D,
    on_error: E,
) -> Result<LoopbackReference>
where
    D: FnMut(&[f32], u16) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let endpoint = match source {
        LoopbackSource::Default(role) => {
            wdev::default_render_endpoint(*role)?.ok_or_else(|| {
                YetiError::Backend("no playback device for loopback reference".into())
            })?
        }
        LoopbackSource::Endpoint(id) => wdev::list_endpoints(Flow::Render)?
            .into_iter()
            .find(|e| &e.id == id)
            .ok_or_else(|| YetiError::Backend(format!("loopback endpoint {id} not active")))?,
    };
    let device = cpal_device(host, &endpoint)?;
    // Loopback must use the render endpoint's own (mix) format.
    let supported = device
        .default_output_config()
        .map_err(|e| cpal_err("loopback mix format", e))?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let ch = config.channels;
            let mut on_data = on_data;
            device.build_input_stream::<f32, _, _>(
                config,
                move |data: &[f32], _| on_data(data, ch),
                on_error,
                None,
            )
        }
        SampleFormat::I16 => build_converting::<i16, _, _>(&device, &config, on_data, on_error),
        SampleFormat::I32 => build_converting::<i32, _, _>(&device, &config, on_data, on_error),
        other => {
            return Err(YetiError::Backend(format!(
                "unsupported loopback mix format {other:?}"
            )));
        }
    }
    .map_err(|e| cpal_err("build loopback stream", e))?;
    stream
        .play()
        .map_err(|e| cpal_err("start loopback stream", e))?;
    Ok(LoopbackReference {
        stream,
        config,
        endpoint,
    })
}

fn build_converting<T, D, E>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut on_data: D,
    on_error: E,
) -> std::result::Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
    D: FnMut(&[f32], u16) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let ch = config.channels.max(1);
    // Whole frames per chunk so `on_data` always sees complete interleaved frames.
    let chunk = CONVERT_CHUNK - CONVERT_CHUNK % ch as usize;
    device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            let mut buf = [0f32; CONVERT_CHUNK];
            for part in data.chunks(chunk.max(1)) {
                for (o, s) in buf.iter_mut().zip(part) {
                    *o = f32::from_sample_(*s);
                }
                on_data(&buf[..part.len()], ch);
            }
        },
        on_error,
        None,
    )
}
