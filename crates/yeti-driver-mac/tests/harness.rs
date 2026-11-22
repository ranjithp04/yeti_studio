//! Pre-install driver gate (PLAN.md Phase 5): load the *built binary* the way coreaudiod
//! would and drive it with a mock host. Nothing is installed.
//!
//! - `dylib_roundtrip`: `dlopen` the cdylib (`$YETI_DRIVER_DYLIB`, default
//!   `target/<profile>/libyeti_driver_mac.dylib`), `dlsym` the factory, run the harness.
//! - `bundle_roundtrip`: if `$YETI_DRIVER_BUNDLE` is set (by `cargo xtask bundle-mac`), load
//!   the signed `YetiStudioMic.driver` through CFPlugIn (Info.plist `CFPlugInTypes` →
//!   factory UUID → `CFPlugInInstanceCreate`), exactly like the HAL, and run the harness.
#![cfg(target_os = "macos")]

#[path = "common/harness.rs"]
mod harness;

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr;

use yeti_driver_mac::ffi::*;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 0x2;
const RTLD_LOCAL: c_int = 0x4;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buf: *const u8,
        len: CFIndex,
        is_dir: Boolean,
    ) -> *const c_void;
    fn CFPlugInCreate(alloc: CFAllocatorRef, url: *const c_void) -> *const c_void;
    fn CFPlugInFindFactoriesForPlugInType(type_uuid: CFUUIDRef) -> *const c_void;
    fn CFArrayGetCount(a: *const c_void) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: CFIndex) -> *const c_void;
    fn CFPlugInInstanceCreate(
        alloc: CFAllocatorRef,
        factory: CFUUIDRef,
        ty: CFUUIDRef,
    ) -> *mut c_void;
}

fn default_dylib() -> PathBuf {
    // CARGO_TARGET_TMPDIR = <target-dir>/tmp; the cdylib is uplifted to <target-dir>/<profile>/.
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    target.join(profile).join("libyeti_driver_mac.dylib")
}

#[test]
fn dylib_roundtrip() {
    let path = std::env::var_os("YETI_DRIVER_DYLIB")
        .map(PathBuf::from)
        .unwrap_or_else(default_dylib);
    assert!(
        path.exists(),
        "driver binary not found at {} (build the cdylib first)",
        path.display()
    );
    let c = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: valid C string; RTLD_LOCAL keeps the image's symbols out of the global namespace.
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
    if h.is_null() {
        // SAFETY: dlerror returns a valid C string after a failed dlopen.
        let e = unsafe { std::ffi::CStr::from_ptr(dlerror()) };
        panic!("dlopen {} failed: {e:?}", path.display());
    }
    let sym = CString::new(yeti_driver_mac::FACTORY_FUNCTION).unwrap();
    // SAFETY: valid handle and C string.
    let f = unsafe { dlsym(h, sym.as_ptr()) };
    assert!(!f.is_null(), "factory symbol not exported");
    // SAFETY: the exported symbol has the CFPlugInFactoryFunction signature.
    let create: harness::Factory =
        unsafe { std::mem::transmute::<*mut c_void, harness::Factory>(f) };
    let drv = harness::create_via_factory(create);
    let r = harness::run(drv, None);
    eprintln!(
        "dylib {}: {} IO cycles, {} loopback frames bit-identical",
        path.display(),
        r.cycles,
        r.frames_compared
    );
    assert!(r.frames_compared > 40_000);
}

#[test]
fn bundle_roundtrip() {
    let Some(bundle) = std::env::var_os("YETI_DRIVER_BUNDLE").map(PathBuf::from) else {
        eprintln!(
            "bundle_roundtrip: YETI_DRIVER_BUNDLE not set — run via `cargo xtask bundle-mac`; nothing checked"
        );
        return;
    };
    assert!(
        bundle.is_dir(),
        "{} is not a bundle directory",
        bundle.display()
    );
    let bytes = bundle.to_str().unwrap().as_bytes();
    // SAFETY: CoreFoundation calls with valid arguments; objects are leaked on purpose (the
    // plug-in must stay loaded for the rest of the process).
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            1,
        );
        assert!(!url.is_null());
        let plugin = CFPlugInCreate(ptr::null(), url);
        assert!(!plugin.is_null(), "CFPlugInCreate failed (Info.plist?)");
        let ty = harness::cfuuid(AUDIO_SERVER_PLUGIN_TYPE_UUID);
        let factories = CFPlugInFindFactoriesForPlugInType(ty);
        assert!(
            !factories.is_null(),
            "no factories registered for kAudioServerPlugInTypeUUID"
        );
        let expected = harness::uuid_from_str(yeti_driver_mac::FACTORY_UUID);
        let mut factory = ptr::null();
        for i in 0..CFArrayGetCount(factories) {
            let u = CFArrayGetValueAtIndex(factories, i);
            if CFUUIDGetUUIDBytes(u) == expected {
                factory = u;
            }
        }
        assert!(
            !factory.is_null(),
            "factory {} not registered by the bundle",
            yeti_driver_mac::FACTORY_UUID
        );
        let iunknown = CFPlugInInstanceCreate(ptr::null(), factory, ty);
        assert!(!iunknown.is_null(), "CFPlugInInstanceCreate returned NULL");
        let r = harness::run(iunknown, None);
        eprintln!(
            "bundle {}: {} IO cycles, {} loopback frames bit-identical",
            bundle.display(),
            r.cycles,
            r.frames_compared
        );
        assert!(r.frames_compared > 40_000);
    }
}
