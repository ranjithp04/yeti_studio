//! A/B test recorder: records raw + cleaned audio side by side on the DSP worker and plays
//! either take back through the monitor ("hear myself") output.
//!
//! The handle side allocates the buffers (`record`); the DSP worker only copies into
//! reserved capacity and never blocks: it uses `try_lock` and skips a block if the UI
//! thread happens to hold the lock for its (tiny) critical section.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use yeti_types::{AbState, AbVariant, ENGINE_SAMPLE_RATE, Result, YetiError};

/// Longest take the UI may request.
pub const MAX_SECONDS: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Recording,
    Ready,
    Playing { variant: AbVariant, pos: usize },
}

struct Inner {
    raw: Vec<f32>,
    clean: Vec<f32>,
    total: usize,
    phase: Phase,
}

pub struct AbRecorder {
    /// Worker fast path: nothing to do while false.
    active: AtomicBool,
    /// Set by the worker when a recording completes; taken by the supervisor (`AbReady`).
    ready: AtomicBool,
    inner: Mutex<Inner>,
}

impl Default for AbRecorder {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                raw: Vec::new(),
                clean: Vec::new(),
                total: 0,
                phase: Phase::Idle,
            }),
        }
    }
}

fn secs(frames: usize) -> f32 {
    frames as f32 / ENGINE_SAMPLE_RATE as f32
}

impl AbRecorder {
    /// Start recording `seconds` of raw + cleaned audio (replaces any previous take).
    pub fn record(&self, seconds: f32) -> Result<()> {
        if !(0.5..=MAX_SECONDS).contains(&seconds) {
            return Err(YetiError::OutOfRange(format!("A/B length {seconds} s")));
        }
        let total = (seconds * ENGINE_SAMPLE_RATE as f32) as usize;
        // Allocate outside the lock so the worker never waits on it.
        let raw = Vec::with_capacity(total);
        let clean = Vec::with_capacity(total);
        let mut g = self.inner.lock().unwrap();
        g.raw = raw;
        g.clean = clean;
        g.total = total;
        g.phase = Phase::Recording;
        self.ready.store(false, Relaxed);
        self.active.store(true, Relaxed);
        Ok(())
    }

    pub fn play(&self, variant: AbVariant) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        match g.phase {
            Phase::Ready | Phase::Playing { .. } if !g.raw.is_empty() => {
                g.phase = Phase::Playing { variant, pos: 0 };
                self.active.store(true, Relaxed);
                Ok(())
            }
            Phase::Recording => Err(YetiError::Backend("A/B take still recording".into())),
            _ => Err(YetiError::Backend("no A/B take recorded".into())),
        }
    }

    pub fn stop(&self) {
        let mut g = self.inner.lock().unwrap();
        match g.phase {
            Phase::Playing { .. } => g.phase = Phase::Ready,
            Phase::Recording => {
                g.phase = Phase::Idle;
                g.raw.clear();
                g.clean.clear();
            }
            _ => {}
        }
        self.active.store(false, Relaxed);
    }

    /// Engine stopped / restarted: abort a recording or playback, keep a finished take.
    pub fn interrupt(&self) {
        let mut g = self.inner.lock().unwrap();
        match g.phase {
            Phase::Recording => {
                g.phase = Phase::Idle;
                g.raw.clear();
                g.clean.clear();
            }
            Phase::Playing { .. } => g.phase = Phase::Ready,
            _ => {}
        }
        self.active.store(false, Relaxed);
    }

    pub fn state(&self) -> AbState {
        let g = self.inner.lock().unwrap();
        match g.phase {
            Phase::Idle => AbState::Idle,
            Phase::Recording => AbState::Recording {
                elapsed_s: secs(g.raw.len()),
                total_s: secs(g.total),
            },
            Phase::Ready => AbState::Ready {
                length_s: secs(g.raw.len()),
            },
            Phase::Playing { variant, pos } => AbState::Playing {
                variant,
                position_s: secs(pos),
            },
        }
    }

    /// A recording finished since the last call.
    pub fn take_ready(&self) -> bool {
        self.ready.swap(false, Relaxed)
    }

    // --- DSP worker side (never blocks, never allocates) ---------------------------

    /// Append one block of raw + cleaned audio while recording.
    pub fn on_block(&self, raw: &[f32], clean: &[f32]) {
        if !self.active.load(Relaxed) {
            return;
        }
        let Ok(mut g) = self.inner.try_lock() else {
            return;
        };
        if g.phase != Phase::Recording {
            return;
        }
        let n = raw.len().min(g.total - g.raw.len());
        g.raw.extend_from_slice(&raw[..n]);
        g.clean.extend_from_slice(&clean[..n]);
        if g.raw.len() >= g.total {
            g.phase = Phase::Ready;
            self.active.store(false, Relaxed);
            self.ready.store(true, Relaxed);
        }
    }

    /// While playing, write the next block of the selected take into `out` and return
    /// `true`; otherwise leave `out` untouched and return `false`.
    pub fn playback(&self, out: &mut [f32]) -> bool {
        if !self.active.load(Relaxed) {
            return false;
        }
        let Ok(mut g) = self.inner.try_lock() else {
            // Keep the monitor fed with silence rather than live audio for this block.
            out.fill(0.0);
            return true;
        };
        let Phase::Playing { variant, pos } = g.phase else {
            return false;
        };
        let take = match variant {
            AbVariant::Raw => &g.raw,
            AbVariant::Cleaned => &g.clean,
        };
        let n = out.len().min(take.len().saturating_sub(pos));
        out[..n].copy_from_slice(&take[pos..pos + n]);
        out[n..].fill(0.0);
        let pos = pos + n;
        if pos >= take.len() {
            g.phase = Phase::Ready;
            self.active.store(false, Relaxed);
        } else {
            g.phase = Phase::Playing { variant, pos };
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_play_both_takes() {
        let ab = AbRecorder::default();
        assert_eq!(ab.state(), AbState::Idle);
        assert!(ab.play(AbVariant::Raw).is_err());
        assert!(ab.record(0.1).is_err());
        ab.record(0.5).unwrap(); // 24000 frames = 50 blocks
        let raw = [0.25f32; 480];
        let clean = [0.5f32; 480];
        for _ in 0..49 {
            ab.on_block(&raw, &clean);
        }
        assert!(matches!(ab.state(), AbState::Recording { .. }));
        assert!(!ab.take_ready());
        ab.on_block(&raw, &clean);
        assert!(ab.take_ready());
        assert_eq!(ab.state(), AbState::Ready { length_s: 0.5 });

        let mut out = [9.0f32; 480];
        ab.play(AbVariant::Cleaned).unwrap();
        assert!(ab.playback(&mut out));
        assert!(out.iter().all(|&x| x == 0.5));
        ab.play(AbVariant::Raw).unwrap();
        for _ in 0..50 {
            assert!(ab.playback(&mut out));
            assert!(out.iter().all(|&x| x == 0.25));
        }
        assert!(!ab.playback(&mut out));
        assert_eq!(ab.state(), AbState::Ready { length_s: 0.5 });
    }

    #[test]
    fn stop_aborts_recording() {
        let ab = AbRecorder::default();
        ab.record(1.0).unwrap();
        ab.on_block(&[0.1; 480], &[0.1; 480]);
        ab.stop();
        assert_eq!(ab.state(), AbState::Idle);
        let mut out = [1.0f32; 480];
        assert!(!ab.playback(&mut out));
        assert_eq!(out[0], 1.0);
    }
}
