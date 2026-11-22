//! Unit tests: the shared host harness run against the driver code linked into this test
//! binary, with a per-thread counting allocator to prove `DoIOOperation` does not allocate.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[path = "../tests/common/harness.rs"]
mod harness;

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator; only bumps a const-initialised thread-local.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarded contract.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(|c| c.get())
}

/// The driver uses process-global state; keep everything touching it in one test.
#[test]
fn in_process_host_harness() {
    let drv = harness::create_via_factory(crate::YetiStudioMic_Create);
    let r = harness::run(drv, Some(allocs));
    assert!(r.frames_compared > 40_000);
    assert_eq!(
        r.allocations_in_io,
        Some(0),
        "Begin/Do/EndIOOperation must not allocate"
    );
}

#[test]
fn counting_allocator_works() {
    let before = allocs();
    let v = std::hint::black_box(vec![1u8; 16]);
    drop(v);
    assert!(allocs() > before);
}

#[test]
fn info_plist_matches_constants() {
    let p = crate::INFO_PLIST;
    for needle in [
        crate::FACTORY_UUID,
        crate::PLUGIN_TYPE_UUID,
        crate::FACTORY_FUNCTION,
        crate::BUNDLE_ID,
        crate::BUNDLE_EXECUTABLE,
    ] {
        assert!(p.contains(needle), "Info.plist missing {needle}");
    }
}
