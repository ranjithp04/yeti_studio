//! In-process AudioServerPlugIn host simulator, shared by:
//! - `src/tests.rs`        (driver code linked directly; also counts allocations in DoIOOperation)
//! - `tests/harness.rs`    (dlopen of the built cdylib, and CFPlugIn loading of the bundle)
//!
//! It drives the driver exactly like coreaudiod does: factory → QueryInterface → Initialize
//! (mock host) → property queries → StartIO → GetZeroTimeStamp → WillDo/Begin/Do/End IO →
//! StopIO, and checks the Feed→Mic loopback returns bit-identical samples.
#![allow(dead_code, non_snake_case, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use yeti_driver_mac::ffi::*;

pub type Factory = unsafe extern "C" fn(CFAllocatorRef, CFUUIDRef) -> *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDCreateFromUUIDBytes(alloc: CFAllocatorRef, bytes: CFUUIDBytes) -> CFUUIDRef;
}

// ---------------------------------------------------------------------------------------------
// Mock host
// ---------------------------------------------------------------------------------------------

pub static HOST_NOTIFICATIONS: AtomicU32 = AtomicU32::new(0);
pub static HOST_LAST_SELECTOR: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn host_properties_changed(
    _h: AudioServerPlugInHostRef,
    _obj: AudioObjectID,
    n: u32,
    addrs: *const AudioObjectPropertyAddress,
) -> OSStatus {
    HOST_NOTIFICATIONS.fetch_add(n, Ordering::SeqCst);
    if n > 0 && !addrs.is_null() {
        // SAFETY: the driver passes `n` valid addresses.
        HOST_LAST_SELECTOR.store(unsafe { (*addrs).mSelector }, Ordering::SeqCst);
    }
    0
}
unsafe extern "C" fn host_copy_from_storage(
    _h: AudioServerPlugInHostRef,
    _k: CFStringRef,
    out: *mut CFTypeRef,
) -> OSStatus {
    if !out.is_null() {
        // SAFETY: valid out-pointer from the caller.
        unsafe { *out = ptr::null() };
    }
    kAudioHardwareUnknownPropertyError
}
unsafe extern "C" fn host_write_to_storage(
    _h: AudioServerPlugInHostRef,
    _k: CFStringRef,
    _d: CFTypeRef,
) -> OSStatus {
    0
}
unsafe extern "C" fn host_delete_from_storage(
    _h: AudioServerPlugInHostRef,
    _k: CFStringRef,
) -> OSStatus {
    0
}
unsafe extern "C" fn host_request_config_change(
    _h: AudioServerPlugInHostRef,
    _d: AudioObjectID,
    _a: u64,
    _i: *mut c_void,
) -> OSStatus {
    0
}

pub static MOCK_HOST: AudioServerPlugInHostInterface = AudioServerPlugInHostInterface {
    PropertiesChanged: host_properties_changed,
    CopyFromStorage: host_copy_from_storage,
    WriteToStorage: host_write_to_storage,
    DeleteFromStorage: host_delete_from_storage,
    RequestDeviceConfigurationChange: host_request_config_change,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

pub fn uuid_from_str(s: &str) -> CFUUIDBytes {
    let hex: Vec<u8> = s.bytes().filter(|b| *b != b'-').collect();
    assert_eq!(hex.len(), 32, "bad uuid {s}");
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        let h = std::str::from_utf8(&hex[i * 2..i * 2 + 2]).unwrap();
        *o = u8::from_str_radix(h, 16).unwrap();
    }
    CFUUIDBytes(out)
}

pub fn cfuuid(b: CFUUIDBytes) -> CFUUIDRef {
    // SAFETY: plain CF constructor.
    let u = unsafe { CFUUIDCreateFromUUIDBytes(ptr::null(), b) };
    assert!(!u.is_null());
    u
}

fn addr(sel: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}
fn glob(sel: u32) -> AudioObjectPropertyAddress {
    addr(sel, kAudioObjectPropertyScopeGlobal)
}

/// Typed access to a driver obtained from the factory.
pub struct Driver {
    pub drv: AudioServerPlugInDriverRef,
}

impl Driver {
    pub fn vt(&self) -> &AudioServerPlugInDriverInterface {
        // SAFETY: drv is a valid driver ref for the lifetime of the test.
        unsafe { &**self.drv }
    }

    pub fn has(&self, obj: AudioObjectID, a: AudioObjectPropertyAddress) -> bool {
        // SAFETY: valid driver + address.
        unsafe { (self.vt().HasProperty)(self.drv, obj, 0, &a) != 0 }
    }

    pub fn size(&self, obj: AudioObjectID, a: AudioObjectPropertyAddress) -> Result<u32, OSStatus> {
        let mut sz = 0u32;
        // SAFETY: valid pointers.
        let st = unsafe {
            (self.vt().GetPropertyDataSize)(self.drv, obj, 0, &a, 0, ptr::null(), &mut sz)
        };
        if st == 0 { Ok(sz) } else { Err(st) }
    }

    pub fn get_raw(
        &self,
        obj: AudioObjectID,
        a: AudioObjectPropertyAddress,
        qual: Option<&CFStringRef>,
        buf: &mut [u8],
    ) -> Result<u32, OSStatus> {
        let (qs, qp) = match qual {
            Some(q) => (
                size_of::<CFStringRef>() as u32,
                (q as *const CFStringRef).cast::<c_void>(),
            ),
            None => (0, ptr::null()),
        };
        let mut out = 0u32;
        // SAFETY: buf is writable for its length.
        let st = unsafe {
            (self.vt().GetPropertyData)(
                self.drv,
                obj,
                0,
                &a,
                qs,
                qp,
                buf.len() as u32,
                &mut out,
                buf.as_mut_ptr().cast(),
            )
        };
        if st == 0 { Ok(out) } else { Err(st) }
    }

    pub fn get<T: Copy>(&self, obj: AudioObjectID, a: AudioObjectPropertyAddress) -> T {
        let mut buf = vec![0u8; size_of::<T>()];
        let n = self.get_raw(obj, a, None, &mut buf).unwrap_or_else(|e| {
            panic!(
                "GetPropertyData obj {obj} sel {:08x} failed: {:08x}",
                a.mSelector, e
            )
        });
        assert_eq!(
            n as usize,
            size_of::<T>(),
            "size for sel {:08x}",
            a.mSelector
        );
        // SAFETY: buffer holds size_of::<T>() bytes written by the driver.
        unsafe { ptr::read_unaligned(buf.as_ptr().cast::<T>()) }
    }

    pub fn u32(&self, obj: AudioObjectID, sel: u32) -> u32 {
        self.get::<u32>(obj, glob(sel))
    }

    pub fn ids(&self, obj: AudioObjectID, a: AudioObjectPropertyAddress) -> Vec<u32> {
        let sz = self.size(obj, a).expect("size");
        let mut buf = vec![0u8; sz as usize];
        let n = self.get_raw(obj, a, None, &mut buf).expect("ids");
        buf[..n as usize]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect()
    }

    pub fn string(&self, obj: AudioObjectID, sel: u32) -> String {
        let cf: CFStringRef = self.get(obj, glob(sel));
        assert!(!cf.is_null());
        let mut buf = [0u8; 512];
        // SAFETY: the driver returned a +1 CFString which we release after copying.
        let s = unsafe { cfstring_to_utf8(cf, &mut buf) }
            .map(|b| String::from_utf8(b.to_vec()).unwrap());
        // SAFETY: we own the returned reference.
        unsafe { CFRelease(cf) };
        s.expect("utf8 string")
    }
}

fn expected_sample(t: i64, ch: usize) -> f32 {
    // Distinct, deterministic, non-zero values per frame and channel.
    let v = (t.rem_euclid(9973) as f32) / 9973.0;
    if ch == 0 { v - 0.5 } else { 0.25 - v * 0.5 }
}

fn cycle(t_in: f64, t_out: f64, frames: u32, counter: u64) -> AudioServerPlugInIOCycleInfo {
    let mut c = AudioServerPlugInIOCycleInfo {
        mIOCycleCounter: counter,
        mNominalIOBufferFrameSize: frames,
        ..Default::default()
    };
    c.mInputTime.mSampleTime = t_in;
    c.mOutputTime.mSampleTime = t_out;
    c.mCurrentTime.mSampleTime = t_in + f64::from(frames);
    c
}

// ---------------------------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------------------------

pub struct Report {
    pub cycles: u64,
    pub frames_compared: u64,
    pub allocations_in_io: Option<u64>,
}

/// Factory checks: wrong type → NULL, right type → driver; returns the IUnknown pointer.
pub fn create_via_factory(create: Factory) -> *mut c_void {
    let wrong = cfuuid(IUNKNOWN_UUID);
    // SAFETY: valid CFUUIDs.
    unsafe {
        assert!(
            create(ptr::null(), wrong).is_null(),
            "factory must reject other plug-in types"
        );
        assert!(
            create(ptr::null(), ptr::null()).is_null(),
            "factory must reject NULL type"
        );
        CFRelease(wrong);
    }
    let ty = cfuuid(AUDIO_SERVER_PLUGIN_TYPE_UUID);
    // SAFETY: valid CFUUID.
    let p = unsafe { create(ptr::null(), ty) };
    // SAFETY: we own `ty`.
    unsafe { CFRelease(ty) };
    assert!(
        !p.is_null(),
        "factory returned NULL for kAudioServerPlugInTypeUUID"
    );
    p
}

/// Full host simulation on an IUnknown pointer returned by the factory.
/// `alloc_count`, if given, returns the number of allocations made so far on this thread.
pub fn run(iunknown: *mut c_void, alloc_count: Option<fn() -> u64>) -> Report {
    // --- COM: QueryInterface / AddRef / Release ---
    // SAFETY: the IUnknown layout is (pointer to) a vtable starting with the IUnknown methods.
    let vt = unsafe { &**(iunknown as AudioServerPlugInDriverRef) };
    let mut out: *mut c_void = ptr::null_mut();
    // SAFETY: valid pointers.
    unsafe {
        assert_eq!(
            (vt.QueryInterface)(
                iunknown,
                AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID,
                &mut out
            ),
            S_OK
        );
        assert_eq!(out, iunknown);
        let mut o2: *mut c_void = ptr::null_mut();
        assert_eq!((vt.QueryInterface)(iunknown, IUNKNOWN_UUID, &mut o2), S_OK);
        assert_eq!(o2, iunknown);
        let mut o3: *mut c_void = ptr::dangling_mut();
        assert_eq!(
            (vt.QueryInterface)(iunknown, AUDIO_SERVER_PLUGIN_TYPE_UUID, &mut o3),
            E_NOINTERFACE
        );
        assert!(o3.is_null());
        let rc = (vt.AddRef)(iunknown);
        assert!(rc >= 2, "AddRef count {rc}");
        assert_eq!((vt.Release)(iunknown), rc - 1);
        (vt.Release)(iunknown); // balance the IUnknown QI
    }
    let d = Driver {
        drv: out as AudioServerPlugInDriverRef,
    };

    // --- Initialize with the mock host ---
    // SAFETY: valid driver + host.
    assert_eq!(unsafe { (d.vt().Initialize)(d.drv, &MOCK_HOST) }, 0);
    // Bad driver ref is rejected.
    let bogus: *const *const AudioServerPlugInDriverInterface = &ptr::null();
    // SAFETY: the driver checks the ref before dereferencing anything.
    assert_eq!(
        unsafe { (d.vt().Initialize)(bogus, &MOCK_HOST) },
        kAudioHardwareBadObjectError
    );

    // --- Plug-in object ---
    let p = kAudioObjectPlugInObject;
    assert_eq!(d.u32(p, kAudioObjectPropertyClass), kAudioPlugInClassID);
    assert_eq!(d.u32(p, kAudioObjectPropertyBaseClass), kAudioObjectClassID);
    let devs = d.ids(p, glob(kAudioPlugInPropertyDeviceList));
    assert_eq!(devs.len(), 2, "device list {devs:?}");
    assert_eq!(d.ids(p, glob(kAudioObjectPropertyOwnedObjects)), devs);
    assert_eq!(
        d.string(p, kAudioObjectPropertyManufacturer),
        yeti_driver_mac::MANUFACTURER
    );

    // Translate UID → device for both UIDs, and an unknown UID → kAudioObjectUnknown.
    let lookup = |uid: &str| -> u32 {
        let s = cfstring(uid);
        let mut buf = [0u8; 4];
        let n = d
            .get_raw(
                p,
                glob(kAudioPlugInPropertyTranslateUIDToDevice),
                Some(&s),
                &mut buf,
            )
            .unwrap();
        // SAFETY: we own `s`.
        unsafe { CFRelease(s) };
        assert_eq!(n, 4);
        u32::from_ne_bytes(buf)
    };
    let mic = lookup(yeti_driver_mac::MIC_DEVICE_UID);
    let feed = lookup(yeti_driver_mac::FEED_DEVICE_UID);
    assert_eq!(lookup("nope"), kAudioObjectUnknown);
    assert!(devs.contains(&mic) && devs.contains(&feed) && mic != feed);

    // --- Devices ---
    for (dev, name, uid, hidden, can_default, is_input) in [
        (
            mic,
            yeti_driver_mac::MIC_DEVICE_NAME,
            yeti_driver_mac::MIC_DEVICE_UID,
            0,
            1,
            true,
        ),
        (
            feed,
            yeti_driver_mac::FEED_DEVICE_NAME,
            yeti_driver_mac::FEED_DEVICE_UID,
            1,
            0,
            false,
        ),
    ] {
        assert_eq!(d.u32(dev, kAudioObjectPropertyClass), kAudioDeviceClassID);
        assert_eq!(d.u32(dev, kAudioObjectPropertyOwner), p);
        assert_eq!(d.string(dev, kAudioObjectPropertyName), name);
        assert_eq!(d.string(dev, kAudioDevicePropertyDeviceUID), uid);
        assert_eq!(
            d.u32(dev, kAudioDevicePropertyIsHidden),
            hidden,
            "{name} IsHidden"
        );
        assert_eq!(
            d.u32(dev, kAudioDevicePropertyDeviceCanBeDefaultDevice),
            can_default,
            "{name} CanBeDefault"
        );
        if !is_input {
            assert_eq!(
                d.u32(dev, kAudioDevicePropertyDeviceCanBeDefaultSystemDevice),
                0
            );
        }
        assert_eq!(
            d.u32(dev, kAudioDevicePropertyTransportType),
            kAudioDeviceTransportTypeVirtual
        );
        assert_eq!(d.u32(dev, kAudioDevicePropertyDeviceIsAlive), 1);
        assert_eq!(d.u32(dev, kAudioDevicePropertyDeviceIsRunning), 0);
        assert_eq!(
            d.get::<f64>(dev, glob(kAudioDevicePropertyNominalSampleRate)),
            48_000.0
        );
        let r: AudioValueRange = d.get(dev, glob(kAudioDevicePropertyAvailableNominalSampleRates));
        assert_eq!((r.mMinimum, r.mMaximum), (48_000.0, 48_000.0));
        assert_eq!(
            d.get::<[u32; 2]>(dev, glob(kAudioDevicePropertyPreferredChannelsForStereo)),
            [1, 2]
        );
        assert!(d.u32(dev, kAudioDevicePropertyZeroTimeStampPeriod) > 0);
        assert_eq!(
            d.u32(dev, kAudioDevicePropertyClockDomain),
            d.u32(
                if dev == mic { feed } else { mic },
                kAudioDevicePropertyClockDomain
            ),
            "both devices share one clock"
        );
        let streams = d.ids(dev, glob(kAudioDevicePropertyStreams));
        assert_eq!(streams.len(), 1);
        let in_streams = d.ids(
            dev,
            addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput),
        );
        let out_streams = d.ids(
            dev,
            addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput),
        );
        assert_eq!(
            (in_streams.len(), out_streams.len()),
            if is_input { (1, 0) } else { (0, 1) }
        );
        assert!(
            !d.has(dev, glob(fourcc(b"zzzz"))),
            "unknown selector must not exist"
        );

        // --- Stream ---
        let s = streams[0];
        assert_eq!(d.u32(s, kAudioObjectPropertyClass), kAudioStreamClassID);
        assert_eq!(d.u32(s, kAudioObjectPropertyOwner), dev);
        assert_eq!(d.u32(s, kAudioStreamPropertyDirection), u32::from(is_input));
        assert_eq!(d.u32(s, kAudioStreamPropertyStartingChannel), 1);
        assert_eq!(d.u32(s, kAudioStreamPropertyIsActive), 1);
        for sel in [
            kAudioStreamPropertyVirtualFormat,
            kAudioStreamPropertyPhysicalFormat,
        ] {
            let f: AudioStreamBasicDescription = d.get(s, glob(sel));
            assert_eq!(f.mSampleRate, 48_000.0);
            assert_eq!(f.mFormatID, kAudioFormatLinearPCM);
            assert_eq!(f.mChannelsPerFrame, 2);
            assert_eq!(f.mBitsPerChannel, 32);
            assert_eq!(f.mBytesPerFrame, 8);
            assert_ne!(f.mFormatFlags & kAudioFormatFlagIsFloat, 0);
            // Setting the same format is accepted, a different one is refused.
            let mut settable = 0u8;
            // SAFETY: valid pointers.
            unsafe {
                assert_eq!(
                    (d.vt().IsPropertySettable)(d.drv, s, 0, &glob(sel), &mut settable),
                    0
                );
                assert_eq!(settable, 1);
                let a = glob(sel);
                let sz = size_of::<AudioStreamBasicDescription>() as u32;
                assert_eq!(
                    (d.vt().SetPropertyData)(
                        d.drv,
                        s,
                        0,
                        &a,
                        0,
                        ptr::null(),
                        sz,
                        (&raw const f).cast()
                    ),
                    0
                );
                let mut g = f;
                g.mSampleRate = 44_100.0;
                assert_eq!(
                    (d.vt().SetPropertyData)(
                        d.drv,
                        s,
                        0,
                        &a,
                        0,
                        ptr::null(),
                        sz,
                        (&raw const g).cast()
                    ),
                    kAudioDeviceUnsupportedFormatError
                );
            }
        }
        let rf: AudioStreamRangedDescription =
            d.get(s, glob(kAudioStreamPropertyAvailableVirtualFormats));
        assert_eq!(rf.mFormat.mSampleRate, 48_000.0);
    }

    // Size errors and bad objects.
    let mut tiny = [0u8; 2];
    assert_eq!(
        d.get_raw(mic, glob(kAudioDevicePropertyIsHidden), None, &mut tiny),
        Err(kAudioHardwareBadPropertySizeError)
    );
    assert!(!d.has(999, glob(kAudioObjectPropertyClass)));
    assert_eq!(
        d.size(999, glob(kAudioObjectPropertyClass)),
        Err(kAudioHardwareBadObjectError)
    );
    let mut settable = 1u8;
    // SAFETY: valid pointers.
    unsafe {
        assert_eq!(
            (d.vt().IsPropertySettable)(
                d.drv,
                mic,
                0,
                &glob(kAudioDevicePropertyIsHidden),
                &mut settable
            ),
            0
        );
    }
    assert_eq!(settable, 0, "IsHidden is read-only");

    // --- IO ---
    let mic_stream = d.ids(mic, glob(kAudioDevicePropertyStreams))[0];
    let feed_stream = d.ids(feed, glob(kAudioDevicePropertyStreams))[0];
    let notes_before = HOST_NOTIFICATIONS.load(Ordering::SeqCst);
    // SAFETY: valid driver.
    unsafe {
        assert_eq!((d.vt().StartIO)(d.drv, feed, 1), 0);
        assert_eq!((d.vt().StartIO)(d.drv, mic, 2), 0);
    }
    assert_eq!(d.u32(mic, kAudioDevicePropertyDeviceIsRunning), 1);
    assert_eq!(d.u32(feed, kAudioDevicePropertyDeviceIsRunning), 1);
    assert!(
        HOST_NOTIFICATIONS.load(Ordering::SeqCst) >= notes_before + 2,
        "host notified of IsRunning"
    );
    assert_eq!(
        HOST_LAST_SELECTOR.load(Ordering::SeqCst),
        kAudioDevicePropertyDeviceIsRunning
    );

    // Zero time stamps are monotonic and sane.
    let zts = |dev| {
        let (mut s, mut h, mut seed) = (0f64, 0u64, 0u64);
        // SAFETY: valid pointers.
        assert_eq!(
            unsafe { (d.vt().GetZeroTimeStamp)(d.drv, dev, 0, &mut s, &mut h, &mut seed) },
            0
        );
        (s, h, seed)
    };
    let (s0, h0, seed0) = zts(mic);
    let (s1, h1, seed1) = zts(feed);
    assert!(s1 >= s0 && h1 >= h0 && seed0 == seed1 && seed0 != 0);
    assert_eq!(
        s0 % f64::from(d.u32(mic, kAudioDevicePropertyZeroTimeStampPeriod)),
        0.0
    );

    // WillDoIOOperation: Feed does WriteMix only, Mic does ReadInput only.
    let will = |dev, op| {
        let (mut w, mut ip) = (9u8, 9u8);
        // SAFETY: valid pointers.
        assert_eq!(
            unsafe { (d.vt().WillDoIOOperation)(d.drv, dev, 0, op, &mut w, &mut ip) },
            0
        );
        w
    };
    assert_eq!(will(feed, kAudioServerPlugInIOOperationWriteMix), 1);
    assert_eq!(will(feed, kAudioServerPlugInIOOperationReadInput), 0);
    assert_eq!(will(mic, kAudioServerPlugInIOOperationReadInput), 1);
    assert_eq!(will(mic, kAudioServerPlugInIOOperationWriteMix), 0);

    // Loopback round trip: write cycles to the Feed at output time t, read the Mic two
    // buffers behind. Runs past the ring size (32768 frames) to exercise wrap-around.
    const FRAMES: u32 = 512;
    const LAG: i64 = 2 * FRAMES as i64;
    let cycles: u64 = 100;
    let start: i64 = 1_000;
    let n = FRAMES as usize * 2;
    let mut wbuf = vec![0f32; n];
    let mut rbuf = vec![0f32; n];
    let mut frames_compared = 0u64;
    let mut allocs = 0u64;
    let v = d.vt();
    for k in 0..cycles {
        let t_out = start + k as i64 * i64::from(FRAMES);
        let t_in = t_out - LAG;
        for (i, fr) in wbuf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            fr[0] = expected_sample(t_out + i as i64, 0);
            fr[1] = expected_sample(t_out + i as i64, 1);
        }
        rbuf.fill(f32::NAN);
        let ci = cycle(t_in as f64, t_out as f64, FRAMES, k + 1);
        let a0 = alloc_count.map(|f| f());
        // SAFETY: buffers hold FRAMES stereo frames; cycle info is valid.
        unsafe {
            let wr = kAudioServerPlugInIOOperationWriteMix;
            let rd = kAudioServerPlugInIOOperationReadInput;
            assert_eq!((v.BeginIOOperation)(d.drv, feed, 1, wr, FRAMES, &ci), 0);
            let st = (v.DoIOOperation)(
                d.drv,
                feed,
                feed_stream,
                1,
                wr,
                FRAMES,
                &ci,
                wbuf.as_mut_ptr().cast(),
                ptr::null_mut(),
            );
            assert_eq!(st, 0);
            assert_eq!((v.EndIOOperation)(d.drv, feed, 1, wr, FRAMES, &ci), 0);
            assert_eq!((v.BeginIOOperation)(d.drv, mic, 2, rd, FRAMES, &ci), 0);
            let st = (v.DoIOOperation)(
                d.drv,
                mic,
                mic_stream,
                2,
                rd,
                FRAMES,
                &ci,
                rbuf.as_mut_ptr().cast(),
                ptr::null_mut(),
            );
            assert_eq!(st, 0);
            assert_eq!((v.EndIOOperation)(d.drv, mic, 2, rd, FRAMES, &ci), 0);
        }
        if let (Some(a0), Some(f)) = (a0, alloc_count) {
            allocs += f() - a0;
        }
        for (i, fr) in rbuf.as_chunks::<2>().0.iter().enumerate() {
            let t = t_in + i as i64;
            if t < start {
                assert_eq!(*fr, [0.0, 0.0], "never-written frame {t} must be silent");
            } else {
                assert_eq!(
                    fr[0].to_bits(),
                    expected_sample(t, 0).to_bits(),
                    "L mismatch at frame {t}"
                );
                assert_eq!(
                    fr[1].to_bits(),
                    expected_sample(t, 1).to_bits(),
                    "R mismatch at frame {t}"
                );
                frames_compared += 1;
            }
        }
    }
    let end = start + cycles as i64 * i64::from(FRAMES);
    assert!(end - start > 32_768, "test must wrap the ring");

    // Reads ahead of the writer, and older than one ring, are silent (no stale audio).
    for t in [end, end - 40_000] {
        let ci = cycle(t as f64, 0.0, FRAMES, 0);
        rbuf.fill(f32::NAN);
        // SAFETY: as above.
        let st = unsafe {
            (v.DoIOOperation)(
                d.drv,
                mic,
                mic_stream,
                2,
                kAudioServerPlugInIOOperationReadInput,
                FRAMES,
                &ci,
                rbuf.as_mut_ptr().cast(),
                ptr::null_mut(),
            )
        };
        assert_eq!(st, 0);
        assert!(
            rbuf.iter().all(|x| *x == 0.0),
            "frames at {t} must be silent"
        );
    }
    // Wrong stream for the device is rejected.
    // SAFETY: as above.
    let st = unsafe {
        let ci = cycle(0.0, 0.0, FRAMES, 0);
        (v.DoIOOperation)(
            d.drv,
            mic,
            feed_stream,
            2,
            kAudioServerPlugInIOOperationReadInput,
            FRAMES,
            &ci,
            rbuf.as_mut_ptr().cast(),
            ptr::null_mut(),
        )
    };
    assert_eq!(st, kAudioHardwareBadStreamError);

    // SAFETY: valid driver.
    unsafe {
        assert_eq!((v.StopIO)(d.drv, mic, 2), 0);
        assert_eq!((v.StopIO)(d.drv, feed, 1), 0);
        assert_eq!((v.StopIO)(d.drv, feed, 1), kAudioHardwareNotRunningError);
    }
    assert_eq!(d.u32(mic, kAudioDevicePropertyDeviceIsRunning), 0);
    assert_eq!(d.u32(feed, kAudioDevicePropertyDeviceIsRunning), 0);

    // After a full stop + restart the ring is cleared: old data must not leak into the Mic.
    // SAFETY: valid driver and buffers.
    unsafe {
        assert_eq!((v.StartIO)(d.drv, mic, 2), 0);
        let ci = cycle((end - LAG) as f64, 0.0, FRAMES, 0);
        rbuf.fill(f32::NAN);
        let st = (v.DoIOOperation)(
            d.drv,
            mic,
            mic_stream,
            2,
            kAudioServerPlugInIOOperationReadInput,
            FRAMES,
            &ci,
            rbuf.as_mut_ptr().cast(),
            ptr::null_mut(),
        );
        assert_eq!(st, 0);
        assert!(
            rbuf.iter().all(|x| *x == 0.0),
            "ring must be cleared on clock restart"
        );
        assert_eq!((v.StopIO)(d.drv, mic, 2), 0);
    }

    Report {
        cycles,
        frames_compared,
        allocations_in_io: alloc_count.map(|_| allocs),
    }
}
