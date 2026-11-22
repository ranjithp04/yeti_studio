//! Real-time scheduling for the DSP worker (PLAN.md §3: "high-priority thread").
//!
//! macOS: Mach time-constraint policy (the class Core Audio uses for its IO threads), so the
//! worker is not delayed by timer coalescing / App Nap while the app is in the background.
//! Other platforms: no-op for now (Windows: MMCSS "Pro Audio" is the Windows engineer's
//! follow-up).

/// Promote the calling thread. `period_ms` = expected wake-up period, `computation_ms` =
/// CPU time needed per period. Returns `true` on success.
pub fn promote_current_thread(period_ms: f64, computation_ms: f64) -> bool {
    imp::promote(period_ms, computation_ms)
}

#[cfg(target_os = "macos")]
mod imp {
    #[repr(C)]
    struct TimeConstraintPolicy {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }
    const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
    const THREAD_TIME_CONSTRAINT_POLICY_COUNT: u32 = 4;

    unsafe extern "C" {
        fn mach_thread_self() -> u32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        fn mach_task_self() -> u32;
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
        fn thread_policy_set(thread: u32, flavor: u32, policy_info: *const u32, count: u32) -> i32;
    }

    pub fn promote(period_ms: f64, computation_ms: f64) -> bool {
        let mut tb = TimebaseInfo::default();
        // SAFETY: plain Mach calls on the current thread with valid out-pointers.
        unsafe {
            if mach_timebase_info(&mut tb) != 0 || tb.numer == 0 {
                return false;
            }
            let ms_to_abs = |ms: f64| (ms * 1.0e6 * tb.denom as f64 / tb.numer as f64) as u32;
            let policy = TimeConstraintPolicy {
                period: ms_to_abs(period_ms),
                computation: ms_to_abs(computation_ms),
                constraint: ms_to_abs(period_ms),
                preemptible: 1,
            };
            let thread = mach_thread_self();
            let r = thread_policy_set(
                thread,
                THREAD_TIME_CONSTRAINT_POLICY,
                &policy as *const TimeConstraintPolicy as *const u32,
                THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            );
            mach_port_deallocate(mach_task_self(), thread);
            r == 0
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn promote(_period_ms: f64, _computation_ms: f64) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn promote_on_a_scratch_thread() {
        let ok = std::thread::spawn(|| super::promote_current_thread(10.0, 3.0))
            .join()
            .unwrap();
        assert_eq!(ok, cfg!(target_os = "macos"));
    }
}
