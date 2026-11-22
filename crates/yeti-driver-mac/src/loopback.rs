//! Shared clock + lock-free loopback ring between the Feed (output) and Mic (input) devices.
//!
//! Real-time rules: everything called from `GetZeroTimeStamp` / `DoIOOperation` uses only
//! atomics and a statically allocated ring — no allocation, no locks, no syscalls
//! (`mach_absolute_time` reads the commpage).
//!
//! Both devices run off ONE clock (same anchor host time), so a frame the HAL writes to the
//! Feed at output sample time `t` is read back by the Mic at input sample time `t`.
//! Validity tracking makes the Mic return silence (not stale audio) for frames that were never
//! written, were written more than one ring length ago, or when the Feed is not running.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::ffi::{MachTimebaseInfo, mach_absolute_time, mach_timebase_info};

pub const SAMPLE_RATE: f64 = 48_000.0;
pub const CHANNELS: usize = 2;
/// `kAudioDevicePropertyZeroTimeStampPeriod`, in frames.
pub const ZTS_PERIOD: u32 = 16_384;
/// Ring capacity in frames (~0.68 s at 48 kHz). Power of two.
pub const RING_FRAMES: usize = 32_768;

const NO_DATA: i64 = i64::MAX;

static RING: [AtomicU32; RING_FRAMES * CHANNELS] =
    [const { AtomicU32::new(0) }; RING_FRAMES * CHANNELS];
/// Exclusive end (sample time) of the newest written data.
static WRITE_END: AtomicI64 = AtomicI64::new(0);
/// First sample time written since the last reset (`NO_DATA` = nothing written).
static VALID_START: AtomicI64 = AtomicI64::new(NO_DATA);

/// Anchor host time of the shared clock (set when the first device starts IO).
static ANCHOR_HOST_TIME: AtomicU64 = AtomicU64::new(0);
/// Host ticks per frame as f64 bits.
static TICKS_PER_FRAME: AtomicU64 = AtomicU64::new(0);

fn ticks_per_frame() -> f64 {
    let bits = TICKS_PER_FRAME.load(Ordering::Relaxed);
    if bits != 0 {
        return f64::from_bits(bits);
    }
    let mut tb = MachTimebaseInfo::default();
    // SAFETY: valid out-pointer.
    unsafe { mach_timebase_info(&mut tb) };
    let (numer, denom) = if tb.numer == 0 || tb.denom == 0 {
        (1, 1)
    } else {
        (tb.numer, tb.denom)
    };
    // ticks = ns * denom / numer
    let t = (1.0e9 / SAMPLE_RATE) * f64::from(denom) / f64::from(numer);
    TICKS_PER_FRAME.store(t.to_bits(), Ordering::Relaxed);
    t
}

/// Called (under the driver's start/stop mutex) when the first device starts IO:
/// restarts the shared clock at "now" and forgets all ring data.
pub fn reset_clock_and_ring() {
    ticks_per_frame();
    // SAFETY: plain FFI call with no arguments.
    let now = unsafe { mach_absolute_time() };
    VALID_START.store(NO_DATA, Ordering::Relaxed);
    WRITE_END.store(0, Ordering::Relaxed);
    ANCHOR_HOST_TIME.store(now, Ordering::Release);
}

/// `GetZeroTimeStamp`: (sample time, host time, seed). RT-safe.
pub fn zero_timestamp() -> (f64, u64, u64) {
    let anchor = ANCHOR_HOST_TIME.load(Ordering::Acquire);
    // SAFETY: plain FFI call with no arguments.
    let now = unsafe { mach_absolute_time() };
    let period_ticks = ticks_per_frame() * f64::from(ZTS_PERIOD);
    let elapsed = now.saturating_sub(anchor) as f64;
    let n = (elapsed / period_ticks).floor();
    let sample_time = n * f64::from(ZTS_PERIOD);
    let host_time = anchor + (n * period_ticks) as u64;
    (sample_time, host_time, 1)
}

#[inline]
fn slot(t: i64) -> usize {
    (t.rem_euclid(RING_FRAMES as i64) as usize) * CHANNELS
}

/// Feed `WriteMix`: store `data` (interleaved stereo f32, `frames` frames) at sample time `t0`.
/// RT-safe. Single writer (the Feed device's IO thread).
pub fn write(t0: i64, data: &[f32]) {
    let frames = (data.len() / CHANNELS) as i64;
    if frames == 0 {
        return;
    }
    let ring = RING_FRAMES as i64;
    let end = WRITE_END.load(Ordering::Relaxed);
    let start = VALID_START.load(Ordering::Relaxed);
    if start == NO_DATA || t0 > end + ring || t0 + frames < end - ring {
        // First write after reset, or a discontinuity larger than the ring: start over.
        VALID_START.store(t0, Ordering::Relaxed);
        WRITE_END.store(t0, Ordering::Release);
    } else if t0 > end {
        // Gap: silence the skipped frames so they are not read back as stale audio.
        for t in end..t0 {
            let s = slot(t);
            for ch in 0..CHANNELS {
                RING[s + ch].store(0, Ordering::Relaxed);
            }
        }
    }
    for (i, frame) in data.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let s = slot(t0 + i as i64);
        for ch in 0..CHANNELS {
            RING[s + ch].store(frame[ch].to_bits(), Ordering::Relaxed);
        }
    }
    let new_end = t0 + frames;
    if new_end > WRITE_END.load(Ordering::Relaxed) {
        WRITE_END.store(new_end, Ordering::Release);
    }
}

/// Mic `ReadInput`: fill `out` (interleaved stereo f32) with the frames at sample time `t0`.
/// Frames that are not valid in the ring read as silence. RT-safe.
pub fn read(t0: i64, out: &mut [f32]) {
    let end = WRITE_END.load(Ordering::Acquire);
    let start = VALID_START.load(Ordering::Relaxed);
    let lo = if start == NO_DATA {
        i64::MAX
    } else {
        start.max(end - RING_FRAMES as i64)
    };
    for (i, frame) in out.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
        let t = t0 + i as i64;
        if t >= lo && t < end {
            let s = slot(t);
            for ch in 0..CHANNELS {
                frame[ch] = f32::from_bits(RING[s + ch].load(Ordering::Relaxed));
            }
        } else {
            frame.fill(0.0);
        }
    }
}
