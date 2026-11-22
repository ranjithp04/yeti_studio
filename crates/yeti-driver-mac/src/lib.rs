//! macOS virtual microphone: AudioServerPlugIn bundle `YetiStudioMic.driver`.
//!
//! Owner: **macOS driver engineer** (Phase 5). Excluded from Windows checks; compiles to an
//! empty library on non-macOS targets.
//!
//! Contract (PLAN.md §3, Phase 5):
//! - 2-ch 48 kHz loopback device. Input side "Yeti Studio Mic"; output side
//!   "Yeti Studio Mic (Feed)" with `IsHidden` and `CanBeDefaultDevice = 0`.
//! - Exports the CFPlugIn factory named in Info.plist (`CFPlugInFactories`).
//! - Every FFI entry point is panic-safe (`catch_unwind` or `panic = "abort"`);
//!   no allocation or locks in `DoIOOperation`.

/// Device names shared with the engine (which routes its output to [`FEED_DEVICE_NAME`]).
pub const MIC_DEVICE_NAME: &str = "Yeti Studio Mic";
pub const FEED_DEVICE_NAME: &str = "Yeti Studio Mic (Feed)";
/// Bundle identifier used in Info.plist.
pub const BUNDLE_ID: &str = "local.yetistudio.driver";

#[cfg(target_os = "macos")]
mod plugin {
    use std::ffi::c_void;

    /// CFPlugIn factory entry point (`AudioServerPlugInDriverInterface` creator).
    ///
    /// Stub: returns NULL, which coreaudiod treats as "no driver". Phase 5 implements
    /// the vtable. Signature: `void* (CFAllocatorRef, CFUUIDRef requestedTypeUUID)`.
    ///
    /// # Safety
    /// Called by the HAL with valid (or NULL) CoreFoundation pointers.
    #[unsafe(no_mangle)]
    #[allow(non_snake_case)]
    pub unsafe extern "C" fn YetiStudioMic_Create(
        _allocator: *const c_void,
        _requested_type_uuid: *const c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }
}

#[cfg(target_os = "macos")]
pub use plugin::YetiStudioMic_Create;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn stub_factory_returns_null() {
        let p = unsafe { super::YetiStudioMic_Create(std::ptr::null(), std::ptr::null()) };
        assert!(p.is_null());
    }
}
