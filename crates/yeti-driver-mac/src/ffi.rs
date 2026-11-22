//! Hand-written FFI for the AudioServerPlugIn interface (CoreAudio/AudioServerPlugIn.h,
//! CoreAudioTypes, CFPlugInCOM.h) and the few CoreFoundation / mach calls the driver needs.
//!
//! `objc2-core-audio` 0.3.2 does not bind `AudioServerPlugInDriverInterface` /
//! `AudioServerPlugInHostInterface` (only a few helper types), so the vtables are declared here.
//! Layouts were copied from the macOS SDK headers and are checked by `const` size asserts below
//! (values from clang `sizeof`/`offsetof` on the macOS 27 SDK, arm64).
#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code
)]

use std::ffi::{c_char, c_void};

pub type OSStatus = i32;
pub type HRESULT = i32;
pub type ULONG = u32;
pub type Boolean = u8;
pub type AudioObjectID = u32;
pub type pid_t = i32;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFIndex = isize;

/// Four-character code, as used by every CoreAudio selector / class / error.
pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

// ---------------------------------------------------------------------------------------------
// COM / CFPlugIn
// ---------------------------------------------------------------------------------------------

/// `CFUUIDBytes` / `REFIID` (passed by value in QueryInterface).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CFUUIDBytes(pub [u8; 16]);

pub const S_OK: HRESULT = 0;
pub const E_NOINTERFACE: HRESULT = 0x8000_0004_u32 as i32;
pub const E_POINTER: HRESULT = 0x8000_0005_u32 as i32;
pub const E_UNEXPECTED: HRESULT = 0x8000_FFFF_u32 as i32;

/// kAudioServerPlugInTypeUUID 443ABAB8-E7B3-491A-B985-BEB9187030DB.
pub const AUDIO_SERVER_PLUGIN_TYPE_UUID: CFUUIDBytes = CFUUIDBytes([
    0x44, 0x3A, 0xBA, 0xB8, 0xE7, 0xB3, 0x49, 0x1A, 0xB9, 0x85, 0xBE, 0xB9, 0x18, 0x70, 0x30, 0xDB,
]);
/// kAudioServerPlugInDriverInterfaceUUID EEA5773D-CC43-49F1-8E00-8F96E7D23B17.
pub const AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID: CFUUIDBytes = CFUUIDBytes([
    0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17,
]);
/// IUnknownUUID 00000000-0000-0000-C000-000000000046.
pub const IUNKNOWN_UUID: CFUUIDBytes = CFUUIDBytes([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
]);

// ---------------------------------------------------------------------------------------------
// CoreAudio types
// ---------------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SMPTETime {
    pub mSubframes: i16,
    pub mSubframeDivisor: i16,
    pub mCounter: u32,
    pub mType: u32,
    pub mFlags: u32,
    pub mHours: i16,
    pub mMinutes: i16,
    pub mSeconds: i16,
    pub mFrames: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioTimeStamp {
    pub mSampleTime: f64,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSMPTETime: SMPTETime,
    pub mFlags: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioServerPlugInIOCycleInfo {
    pub mIOCycleCounter: u64,
    pub mNominalIOBufferFrameSize: u32,
    pub mCurrentTime: AudioTimeStamp,
    pub mInputTime: AudioTimeStamp,
    pub mOutputTime: AudioTimeStamp,
    pub mMainHostTicksPerFrame: f64,
    pub mDeviceHostTicksPerFrame: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioServerPlugInClientInfo {
    pub mClientID: u32,
    pub mProcessID: pid_t,
    pub mIsNativeEndian: Boolean,
    pub mBundleID: CFStringRef,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioValueRange {
    pub mMinimum: f64,
    pub mMaximum: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStreamRangedDescription {
    pub mFormat: AudioStreamBasicDescription,
    pub mSampleRateRange: AudioValueRange,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioChannelDescription {
    pub mChannelLabel: u32,
    pub mChannelFlags: u32,
    pub mCoordinates: [f32; 3],
}

/// `AudioChannelLayout` with exactly two channel descriptions (the C struct is variable-length).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioChannelLayoutStereo {
    pub mChannelLayoutTag: u32,
    pub mChannelBitmap: u32,
    pub mNumberChannelDescriptions: u32,
    pub mChannelDescriptions: [AudioChannelDescription; 2],
}

pub type AudioServerPlugInHostRef = *const AudioServerPlugInHostInterface;
pub type AudioServerPlugInDriverRef = *const *const AudioServerPlugInDriverInterface;

#[repr(C)]
pub struct AudioServerPlugInHostInterface {
    pub PropertiesChanged: unsafe extern "C" fn(
        AudioServerPlugInHostRef,
        AudioObjectID,
        u32,
        *const AudioObjectPropertyAddress,
    ) -> OSStatus,
    pub CopyFromStorage:
        unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef, *mut CFTypeRef) -> OSStatus,
    pub WriteToStorage:
        unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef, CFTypeRef) -> OSStatus,
    pub DeleteFromStorage: unsafe extern "C" fn(AudioServerPlugInHostRef, CFStringRef) -> OSStatus,
    pub RequestDeviceConfigurationChange:
        unsafe extern "C" fn(AudioServerPlugInHostRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
}

pub type DrvRef = AudioServerPlugInDriverRef;
pub type Addr = *const AudioObjectPropertyAddress;

#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    pub _reserved: *mut c_void,
    pub QueryInterface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Release: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Initialize: unsafe extern "C" fn(DrvRef, AudioServerPlugInHostRef) -> OSStatus,
    pub CreateDevice: unsafe extern "C" fn(
        DrvRef,
        CFDictionaryRef,
        *const AudioServerPlugInClientInfo,
        *mut AudioObjectID,
    ) -> OSStatus,
    pub DestroyDevice: unsafe extern "C" fn(DrvRef, AudioObjectID) -> OSStatus,
    pub AddDeviceClient:
        unsafe extern "C" fn(DrvRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub RemoveDeviceClient:
        unsafe extern "C" fn(DrvRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub PerformDeviceConfigurationChange:
        unsafe extern "C" fn(DrvRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub AbortDeviceConfigurationChange:
        unsafe extern "C" fn(DrvRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub HasProperty: unsafe extern "C" fn(DrvRef, AudioObjectID, pid_t, Addr) -> Boolean,
    pub IsPropertySettable:
        unsafe extern "C" fn(DrvRef, AudioObjectID, pid_t, Addr, *mut Boolean) -> OSStatus,
    pub GetPropertyDataSize: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        pid_t,
        Addr,
        u32,
        *const c_void,
        *mut u32,
    ) -> OSStatus,
    pub GetPropertyData: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        pid_t,
        Addr,
        u32,
        *const c_void,
        u32,
        *mut u32,
        *mut c_void,
    ) -> OSStatus,
    pub SetPropertyData: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        pid_t,
        Addr,
        u32,
        *const c_void,
        u32,
        *const c_void,
    ) -> OSStatus,
    pub StartIO: unsafe extern "C" fn(DrvRef, AudioObjectID, u32) -> OSStatus,
    pub StopIO: unsafe extern "C" fn(DrvRef, AudioObjectID, u32) -> OSStatus,
    pub GetZeroTimeStamp:
        unsafe extern "C" fn(DrvRef, AudioObjectID, u32, *mut f64, *mut u64, *mut u64) -> OSStatus,
    pub WillDoIOOperation: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        u32,
        u32,
        *mut Boolean,
        *mut Boolean,
    ) -> OSStatus,
    pub BeginIOOperation: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
    pub DoIOOperation: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
        *mut c_void,
        *mut c_void,
    ) -> OSStatus,
    pub EndIOOperation: unsafe extern "C" fn(
        DrvRef,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
    ) -> OSStatus,
}

// Layout checks (sizes from the SDK headers on arm64/x86_64).
const _: () = assert!(size_of::<AudioObjectPropertyAddress>() == 12);
const _: () = assert!(size_of::<SMPTETime>() == 24);
const _: () = assert!(size_of::<AudioTimeStamp>() == 64);
const _: () = assert!(size_of::<AudioServerPlugInIOCycleInfo>() == 224);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mCurrentTime) == 16);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mInputTime) == 80);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mOutputTime) == 144);
const _: () =
    assert!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mMainHostTicksPerFrame) == 208);
const _: () = assert!(size_of::<AudioServerPlugInClientInfo>() == 24);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInClientInfo, mBundleID) == 16);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInDriverInterface, HasProperty) == 88);
const _: () = assert!(std::mem::offset_of!(AudioServerPlugInDriverInterface, DoIOOperation) == 168);
const _: () = assert!(size_of::<AudioStreamBasicDescription>() == 40);
const _: () = assert!(size_of::<AudioStreamRangedDescription>() == 56);
const _: () = assert!(size_of::<AudioChannelLayoutStereo>() == 52);
const _: () = assert!(size_of::<AudioServerPlugInDriverInterface>() == 23 * 8);
const _: () = assert!(size_of::<AudioServerPlugInHostInterface>() == 5 * 8);

// ---------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------

pub const kAudioObjectUnknown: AudioObjectID = 0;
pub const kAudioObjectPlugInObject: AudioObjectID = 1;

pub const kAudioHardwareNoError: OSStatus = 0;
pub const kAudioHardwareNotRunningError: OSStatus = fourcc(b"stop") as i32;
pub const kAudioHardwareUnspecifiedError: OSStatus = fourcc(b"what") as i32;
pub const kAudioHardwareUnknownPropertyError: OSStatus = fourcc(b"who?") as i32;
pub const kAudioHardwareBadPropertySizeError: OSStatus = fourcc(b"!siz") as i32;
pub const kAudioHardwareIllegalOperationError: OSStatus = fourcc(b"nope") as i32;
pub const kAudioHardwareBadObjectError: OSStatus = fourcc(b"!obj") as i32;
pub const kAudioHardwareBadDeviceError: OSStatus = fourcc(b"!dev") as i32;
pub const kAudioHardwareBadStreamError: OSStatus = fourcc(b"!str") as i32;
pub const kAudioHardwareUnsupportedOperationError: OSStatus = fourcc(b"unop") as i32;
pub const kAudioDeviceUnsupportedFormatError: OSStatus = fourcc(b"!dat") as i32;

pub const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
pub const kAudioObjectPropertyScopeInput: u32 = fourcc(b"inpt");
pub const kAudioObjectPropertyScopeOutput: u32 = fourcc(b"outp");
pub const kAudioObjectPropertyElementMain: u32 = 0;

pub const kAudioObjectClassID: u32 = fourcc(b"aobj");
pub const kAudioPlugInClassID: u32 = fourcc(b"aplg");
pub const kAudioDeviceClassID: u32 = fourcc(b"adev");
pub const kAudioStreamClassID: u32 = fourcc(b"astr");

pub const kAudioObjectPropertyBaseClass: u32 = fourcc(b"bcls");
pub const kAudioObjectPropertyClass: u32 = fourcc(b"clas");
pub const kAudioObjectPropertyOwner: u32 = fourcc(b"stdv");
pub const kAudioObjectPropertyName: u32 = fourcc(b"lnam");
pub const kAudioObjectPropertyManufacturer: u32 = fourcc(b"lmak");
pub const kAudioObjectPropertyOwnedObjects: u32 = fourcc(b"ownd");
pub const kAudioObjectPropertyControlList: u32 = fourcc(b"ctrl");

pub const kAudioPlugInPropertyDeviceList: u32 = fourcc(b"dev#");
pub const kAudioPlugInPropertyTranslateUIDToDevice: u32 = fourcc(b"uidd");
pub const kAudioPlugInPropertyResourceBundle: u32 = fourcc(b"rsrc");

pub const kAudioDevicePropertyDeviceUID: u32 = fourcc(b"uid ");
pub const kAudioDevicePropertyModelUID: u32 = fourcc(b"muid");
pub const kAudioDevicePropertyTransportType: u32 = fourcc(b"tran");
pub const kAudioDevicePropertyRelatedDevices: u32 = fourcc(b"akin");
pub const kAudioDevicePropertyClockDomain: u32 = fourcc(b"clkd");
pub const kAudioDevicePropertyDeviceIsAlive: u32 = fourcc(b"livn");
pub const kAudioDevicePropertyDeviceIsRunning: u32 = fourcc(b"goin");
pub const kAudioDevicePropertyDeviceCanBeDefaultDevice: u32 = fourcc(b"dflt");
pub const kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: u32 = fourcc(b"sflt");
pub const kAudioDevicePropertyLatency: u32 = fourcc(b"ltnc");
pub const kAudioDevicePropertyStreams: u32 = fourcc(b"stm#");
pub const kAudioDevicePropertySafetyOffset: u32 = fourcc(b"saft");
pub const kAudioDevicePropertyNominalSampleRate: u32 = fourcc(b"nsrt");
pub const kAudioDevicePropertyAvailableNominalSampleRates: u32 = fourcc(b"nsr#");
pub const kAudioDevicePropertyIsHidden: u32 = fourcc(b"hidn");
pub const kAudioDevicePropertyPreferredChannelsForStereo: u32 = fourcc(b"dch2");
pub const kAudioDevicePropertyPreferredChannelLayout: u32 = fourcc(b"srnd");
pub const kAudioDevicePropertyZeroTimeStampPeriod: u32 = fourcc(b"ring");
pub const kAudioDeviceTransportTypeVirtual: u32 = fourcc(b"virt");

pub const kAudioStreamPropertyIsActive: u32 = fourcc(b"sact");
pub const kAudioStreamPropertyDirection: u32 = fourcc(b"sdir");
pub const kAudioStreamPropertyTerminalType: u32 = fourcc(b"term");
pub const kAudioStreamPropertyStartingChannel: u32 = fourcc(b"schn");
pub const kAudioStreamPropertyLatency: u32 = kAudioDevicePropertyLatency;
pub const kAudioStreamPropertyVirtualFormat: u32 = fourcc(b"sfmt");
pub const kAudioStreamPropertyAvailableVirtualFormats: u32 = fourcc(b"sfma");
pub const kAudioStreamPropertyPhysicalFormat: u32 = fourcc(b"pft ");
pub const kAudioStreamPropertyAvailablePhysicalFormats: u32 = fourcc(b"pfta");
pub const kAudioStreamTerminalTypeMicrophone: u32 = fourcc(b"micr");
pub const kAudioStreamTerminalTypeLine: u32 = fourcc(b"line");

pub const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
pub const kAudioFormatFlagIsFloat: u32 = 1 << 0;
pub const kAudioFormatFlagIsPacked: u32 = 1 << 3;
/// Native endian on Apple silicon / Intel = little endian = no BigEndian flag.
pub const kAudioFormatFlagsNativeFloatPacked: u32 =
    kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;

pub const kAudioChannelLayoutTag_UseChannelDescriptions: u32 = 0;
pub const kAudioChannelLabel_Left: u32 = 1;
pub const kAudioChannelLabel_Right: u32 = 2;

pub const kAudioServerPlugInIOOperationReadInput: u32 = fourcc(b"read");
pub const kAudioServerPlugInIOOperationWriteMix: u32 = fourcc(b"rite");

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

// ---------------------------------------------------------------------------------------------
// System functions
// ---------------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: Boolean,
    ) -> CFStringRef;
    pub fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    pub fn CFRelease(cf: CFTypeRef);
}

unsafe extern "C" {
    pub fn mach_absolute_time() -> u64;
    pub fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// Creates a +1 CFString (caller releases), or NULL on failure.
pub fn cfstring(s: &str) -> CFStringRef {
    // SAFETY: pointer/length describe a valid UTF-8 buffer for the duration of the call.
    unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        )
    }
}

/// Copies a CFString into `buf` as UTF-8 and returns the bytes, or `None` if it does not fit.
///
/// # Safety
/// `s` must be NULL or a valid CFStringRef.
pub unsafe fn cfstring_to_utf8(s: CFStringRef, buf: &mut [u8]) -> Option<&[u8]> {
    if s.is_null() {
        return None;
    }
    // SAFETY: `s` is a valid CFString (caller contract); buf is writable for its length.
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr().cast(),
            buf.len() as CFIndex,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0)?;
    Some(&buf[..len])
}
