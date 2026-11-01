//! # DirectInput8 to XInput Controller Discovery Bridge (`dinput8.dll`)
//!
//! ## Problem Statement
//! On Windows, an XInput-compatible controller (such as an 8BitDo Wired/USB Controller,
//! Xbox 360/One/Series controller, or a controller managed by HidHide / DS4Windows) exposes
//! two separate device nodes in the Windows Plug and Play (PnP) device tree:
//!
//! 1. **The XUSB / XInput interface** (`USB\VID_xxxx&PID_xxxx&MI_00` / `USB\VID_xxxx&PID_xxxx&IG_00`),
//!    which provides controller state through `xinput9_1_0.dll` / `xinput1_4.dll`.
//! 2. **A child DirectInput HID node** (`HID-compliant game controller`,
//!    `HID\VID_xxxx&PID_xxxx&IG_00\...`), which exposes the controller to legacy DirectInput8
//!    enumeration (`IDirectInput8::EnumDevices`).
//!
//! Applications that implement Microsoft's classic DirectX SDK *"XInput and DirectInput"*
//! enumeration pattern (`IsXInputDevice`) use a two-stage controller discovery pipeline:
//!
//! - **Stage 1 (DirectInput8 Enumeration):** The application calls `DirectInput8Create`
//!   (`IID_IDirectInput8A` or `IID_IDirectInput8W`) and invokes
//!   `IDirectInput8::EnumDevices(DI8DEVCLASS_GAMECTRL /* 4 */, ...)` with `DIEDFL_ATTACHEDONLY` (`1`).
//! - **Stage 2 (WMI `IG_` Verification & XInput Binding):** Inside the `EnumDevices` callback,
//!   the application runs Microsoft's `IsXInputDevice(&lpddi->guidProduct)` check, which queries
//!   WMI (`Win32_PNPEntity`) for any PnP `DeviceID` containing `"IG_"`, parses `VID_%4X` and
//!   `PID_%4X`, computes `MAKELONG(VID, PID)` (`(PID << 16) | VID`), and compares it against
//!   `lpddi->guidProduct.Data1`. Only after that check succeeds does the application poll
//!   `XInputGetCapabilities(0..3)` and bind the active XInput slot.
//!
//! If the child `HID-compliant game controller` node is disabled in Windows Device Manager
//! (`CM_PROB_DISABLED` / Error Code 22) or hidden by input-hiding utilities (such as HidHide
//! to prevent double-input in other games), `IDirectInput8::EnumDevices` returns **0
//! controllers**. As a result, the application never reaches Stage 2, never checks XInput,
//! and fails to detect the controller.
//!
//! ## Solution Overview
//! This crate compiles to a drop-in `dinput8.dll` proxy placed in the application's executable
//! directory:
//!
//! 1. All standard `dinput8.dll` exports (`DirectInput8Create`, `DllCanUnloadNow`,
//!    `DllGetClassObject`, `DllRegisterServer`, `DllUnregisterServer`, `GetdfDIJoystick`)
//!    are transparently forwarded to the genuine system library in `C:\Windows\System32\dinput8.dll`.
//! 2. When `DirectInput8Create` returns an `IDirectInput8A` or `IDirectInput8W` interface,
//!    vtable slot 4 (`EnumDevices`) is hooked.
//! 3. When `EnumDevices` is called for game controllers (`DI8DEVCLASS_GAMECTRL` or
//!    `DI8DEVCLASS_ALL`):
//!    - First, the genuine `EnumDevices` method is invoked. A lightweight wrapper callback
//!      observes whether Windows already reported any gamepad device. If Windows already
//!      enumerated a controller, this proxy does nothing further, preventing duplicate inputs.
//!    - Second, if DirectInput reported zero gamepads, the proxy checks `XInputGetCapabilities`
//!      across XInput slots `0..3`.
//!    - For each active XInput slot, the proxy dynamically queries `SetupDiGetClassDevsW`
//!      for present PnP devices containing `"IG_"` to extract the controller's hardware
//!      `VID` and `PID` into `MAKELONG(VID, PID)` (matching what `IsXInputDevice` expects
//!      in WMI), synthesizes a standard `DIDEVICEINSTANCEA` / `DIDEVICEINSTANCEW` structure,
//!      and invokes the application's callback.

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

// ============================================================================
// COM & Win32 Data Structures
// ============================================================================

/// Standard Win32 / COM 128-bit globally unique identifier (`GUID` / `IID`).
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

/// `IID_IDirectInput8A` (`{BF798030-483A-4DA2-AA99-5D64ED369700}`).
const IID_IDIRECTINPUT8A: GUID = GUID {
    Data1: 0xBF798030,
    Data2: 0x483A,
    Data3: 0x4DA2,
    Data4: [0xAA, 0x99, 0x5D, 0x64, 0xED, 0x36, 0x97, 0x00],
};

/// `IID_IDirectInput8W` (`{BF798031-483A-4DA2-AA99-5D64ED369700}`).
const IID_IDIRECTINPUT8W: GUID = GUID {
    Data1: 0xBF798031,
    Data2: 0x483A,
    Data3: 0x4DA2,
    Data4: [0xAA, 0x99, 0x5D, 0x64, 0xED, 0x36, 0x97, 0x00],
};

/// Default fallback `MAKELONG(VID, PID)` for 8BitDo USB Wired Controller (`VID_2DC8&PID_310A`).
const DEFAULT_8BITDO_VID_PID: u32 = 0x310A_2DC8;

/// DirectInput8 `DIDEVICEINSTANCEA` structure (ANSI, 580 bytes).
/// Passed to the `LPDIENUMDEVICESCALLBACKA` callback during `IDirectInput8A::EnumDevices`.
#[repr(C)]
pub struct DIDEVICEINSTANCEA {
    pub dwSize: u32,
    pub guidInstance: GUID,
    pub guidProduct: GUID,
    pub dwDevType: u32,
    pub tszInstanceName: [u8; 260],
    pub tszProductName: [u8; 260],
    pub guidFFDriver: GUID,
    pub wUsagePage: u16,
    pub wUsage: u16,
}

/// DirectInput8 `DIDEVICEINSTANCEW` structure (Unicode UTF-16LE, 1100 bytes).
/// Passed to the `LPDIENUMDEVICESCALLBACKW` callback during `IDirectInput8W::EnumDevices`.
#[repr(C)]
pub struct DIDEVICEINSTANCEW {
    pub dwSize: u32,
    pub guidInstance: GUID,
    pub guidProduct: GUID,
    pub dwDevType: u32,
    pub tszInstanceName: [u16; 260],
    pub tszProductName: [u16; 260],
    pub guidFFDriver: GUID,
    pub wUsagePage: u16,
    pub wUsage: u16,
}

// Compile-time ABI size assertions matching the Microsoft DirectX 8 SDK headers.
const _: () = assert!(std::mem::size_of::<GUID>() == 16);
const _: () = assert!(std::mem::size_of::<DIDEVICEINSTANCEA>() == 580);
const _: () = assert!(std::mem::size_of::<DIDEVICEINSTANCEW>() == 1100);

/// XInput `XINPUT_CAPABILITIES` structure returned by `XInputGetCapabilities`.
#[repr(C)]
pub struct XINPUT_CAPABILITIES {
    pub Type: u8,
    pub SubType: u8,
    pub Flags: u16,
    pub wButtons: u16,
    pub bLeftTrigger: u8,
    pub bRightTrigger: u8,
    pub sThumbLX: i16,
    pub sThumbLY: i16,
    pub sThumbRX: i16,
    pub sThumbRY: i16,
    pub wLeftMotorSpeed: u16,
    pub wRightMotorSpeed: u16,
}

/// SetupAPI `SP_DEVINFO_DATA` structure used to enumerate PnP device nodes.
#[repr(C)]
pub struct SP_DEVINFO_DATA {
    pub cbSize: u32,
    pub ClassGuid: GUID,
    pub DevInst: u32,
    pub Reserved: usize,
}

// ============================================================================
// Function Pointer Signatures & External Win32 Imports
// ============================================================================

type DirectInput8CreateFn = unsafe extern "system" fn(
    hinst: *mut c_void,
    dwVersion: u32,
    riidltf: *const GUID,
    ppvOut: *mut *mut c_void,
    punkOuter: *mut c_void,
) -> i32;

type DllCanUnloadNowFn = unsafe extern "system" fn() -> i32;
type DllGetClassObjectFn =
    unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> i32;
type DllRegisterServerFn = unsafe extern "system" fn() -> i32;
type DllUnregisterServerFn = unsafe extern "system" fn() -> i32;
type GetdfDIJoystickFn = unsafe extern "system" fn() -> *const c_void;

type DIEnumDevicesCallbackA =
    unsafe extern "system" fn(*const DIDEVICEINSTANCEA, *mut c_void) -> i32;
type DIEnumDevicesCallbackW =
    unsafe extern "system" fn(*const DIDEVICEINSTANCEW, *mut c_void) -> i32;

type EnumDevicesAFn = unsafe extern "system" fn(
    pThis: *mut c_void,
    dwDevType: u32,
    lpCallback: DIEnumDevicesCallbackA,
    pvRef: *mut c_void,
    dwFlags: u32,
) -> i32;

type EnumDevicesWFn = unsafe extern "system" fn(
    pThis: *mut c_void,
    dwDevType: u32,
    lpCallback: DIEnumDevicesCallbackW,
    pvRef: *mut c_void,
    dwFlags: u32,
) -> i32;

type XInputGetCapabilitiesFn = unsafe extern "system" fn(u32, u32, *mut XINPUT_CAPABILITIES) -> u32;

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemDirectoryW(lpBuffer: *mut u16, uSize: u32) -> u32;
    fn LoadLibraryW(lpLibFileName: *const u16) -> *mut c_void;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
    fn VirtualProtect(
        lpAddress: *mut c_void,
        dwSize: usize,
        flNewProtect: u32,
        lpflOldProtect: *mut u32,
    ) -> i32;
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        ClassGuid: *const GUID,
        Enumerator: *const u16,
        hwndParent: *mut c_void,
        Flags: u32,
    ) -> *mut c_void;
    fn SetupDiEnumDeviceInfo(
        DeviceInfoSet: *mut c_void,
        MemberIndex: u32,
        DeviceInfoData: *mut SP_DEVINFO_DATA,
    ) -> i32;
    fn SetupDiGetDeviceInstanceIdW(
        DeviceInfoSet: *mut c_void,
        DeviceInfoData: *mut SP_DEVINFO_DATA,
        DeviceInstanceId: *mut u16,
        DeviceInstanceIdSize: u32,
        RequiredSize: *mut u32,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(DeviceInfoSet: *mut c_void) -> i32;
}

const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_ALLCLASSES: u32 = 0x0000_0004;
const INVALID_HANDLE_VALUE: isize = -1;

/// Standard DirectInput8 HID Gamepad device type (`0x00010215`):
/// - Bit 16 (`0x0001_0000`): `DIDEVTYPE_HID`
/// - Subtype (`0x0000_0200`): `DI8DEVTYPEGAMEPAD_STANDARD` (`2`)
/// - Type (`0x0000_0015`): `DI8DEVTYPE_GAMEPAD` (`21`)
const GAMEPAD_DEV_TYPE: u32 = 0x0001_0215;

// ============================================================================
// Global State
// ============================================================================

/// Cached handle to the genuine `C:\Windows\System32\dinput8.dll`.
static REAL_DINPUT8: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Original `IDirectInput8A::EnumDevices` function pointer from the system vtable.
static ORIG_ENUM_A: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Original `IDirectInput8W::EnumDevices` function pointer from the system vtable.
static ORIG_ENUM_W: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Cached `XInputGetCapabilities` function pointer resolved once per process.
static XINPUT_GET_CAPS: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

// ============================================================================
// Helper Functions
// ============================================================================

/// Loads the genuine `dinput8.dll` from the Windows System32 directory.
unsafe fn get_real_dinput8() -> *mut c_void {
    let cur = REAL_DINPUT8.load(Ordering::Acquire);
    if !cur.is_null() {
        return cur;
    }

    let mut buf = [0u16; 512];
    let len = GetSystemDirectoryW(buf.as_mut_ptr(), 480) as usize;
    if len == 0 || len > 480 {
        return ptr::null_mut();
    }
    let suffix: [u16; 13] = [
        b'\\' as u16,
        b'd' as u16,
        b'i' as u16,
        b'n' as u16,
        b'p' as u16,
        b'u' as u16,
        b't' as u16,
        b'8' as u16,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
        0,
    ];
    for (i, &ch) in suffix.iter().enumerate() {
        buf[len + i] = ch;
    }

    let hmod = LoadLibraryW(buf.as_ptr());
    if !hmod.is_null() {
        REAL_DINPUT8.store(hmod, Ordering::Release);
    }
    hmod
}

/// Resolves and caches `XInputGetCapabilities` from `xinput9_1_0.dll` or `xinput1_4.dll`.
unsafe fn get_xinput_caps_fn() -> Option<XInputGetCapabilitiesFn> {
    let cached = XINPUT_GET_CAPS.load(Ordering::Acquire);
    if !cached.is_null() {
        return Some(std::mem::transmute(cached));
    }

    let dll_910: [u16; 16] = [
        b'x' as u16,
        b'i' as u16,
        b'n' as u16,
        b'p' as u16,
        b'u' as u16,
        b't' as u16,
        b'9' as u16,
        b'_' as u16,
        b'1' as u16,
        b'_' as u16,
        b'0' as u16,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
        0,
    ];
    let mut hmod = LoadLibraryW(dll_910.as_ptr());
    if hmod.is_null() {
        let dll_14: [u16; 14] = [
            b'x' as u16,
            b'i' as u16,
            b'n' as u16,
            b'p' as u16,
            b'u' as u16,
            b't' as u16,
            b'1' as u16,
            b'_' as u16,
            b'4' as u16,
            b'.' as u16,
            b'd' as u16,
            b'l' as u16,
            b'l' as u16,
            0,
        ];
        hmod = LoadLibraryW(dll_14.as_ptr());
    }
    if hmod.is_null() {
        return None;
    }

    let proc = GetProcAddress(hmod, b"XInputGetCapabilities\0".as_ptr());
    if proc.is_null() {
        None
    } else {
        XINPUT_GET_CAPS.store(proc, Ordering::Release);
        Some(std::mem::transmute(proc))
    }
}

/// Parses a 4-digit hexadecimal number from a UTF-16 slice (case-insensitive).
fn parse_hex4_u16(slice: &[u16]) -> Option<u16> {
    if slice.len() < 4 {
        return None;
    }
    let mut val: u16 = 0;
    for &ch in &slice[..4] {
        let digit = match ch as u8 {
            b'0'..=b'9' => (ch as u8) - b'0',
            b'a'..=b'f' => (ch as u8) - b'a' + 10,
            b'A'..=b'F' => (ch as u8) - b'A' + 10,
            _ => return None,
        };
        val = (val << 4) | (digit as u16);
    }
    Some(val)
}

/// Searches for an ASCII pattern (case-insensitive) inside a UTF-16 slice and returns
/// the index immediately following the matched pattern.
fn find_subslice_after_ascii(haystack: &[u16], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    'outer: for i in 0..=(haystack.len() - needle.len()) {
        for (j, &nb) in needle.iter().enumerate() {
            let hb = (haystack[i + j] & 0xFF) as u8;
            let hb_upper = if hb >= b'a' && hb <= b'z' {
                hb - 32
            } else {
                hb
            };
            let nb_upper = if nb >= b'a' && nb <= b'z' {
                nb - 32
            } else {
                nb
            };
            if hb_upper != nb_upper {
                continue 'outer;
            }
        }
        return Some(i + needle.len());
    }
    None
}

/// Dynamically discovers the `MAKELONG(VID, PID)` (`(PID << 16) | VID`) of any present
/// XInput PnP device node whose Instance ID contains `"IG_"` (e.g.,
/// `USB\VID_2DC8&PID_310A&IG_00\...`).
///
/// This mirrors Microsoft's documented `IsXInputDevice` WMI (`Win32_PNPEntity`) check,
/// ensuring that `lpddi->guidProduct.Data1` matches any connected XInput controller
/// (8BitDo, Xbox, GameSir, GuliKit, etc.).
unsafe fn discover_xinput_vid_pid() -> u32 {
    let dev_info = SetupDiGetClassDevsW(
        ptr::null(),
        ptr::null(),
        ptr::null_mut(),
        DIGCF_ALLCLASSES | DIGCF_PRESENT,
    );
    if (dev_info as isize) == INVALID_HANDLE_VALUE || dev_info.is_null() {
        return DEFAULT_8BITDO_VID_PID;
    }

    let mut result = DEFAULT_8BITDO_VID_PID;
    let mut index = 0u32;
    let mut dev_data = SP_DEVINFO_DATA {
        cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
        ClassGuid: GUID {
            Data1: 0,
            Data2: 0,
            Data3: 0,
            Data4: [0; 8],
        },
        DevInst: 0,
        Reserved: 0,
    };

    let mut id_buf = [0u16; 512];
    while SetupDiEnumDeviceInfo(dev_info, index, &mut dev_data) != 0 {
        index += 1;
        let mut req_size = 0u32;
        if SetupDiGetDeviceInstanceIdW(
            dev_info,
            &mut dev_data,
            id_buf.as_mut_ptr(),
            id_buf.len() as u32,
            &mut req_size,
        ) != 0
        {
            let len = id_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let slice = &id_buf[..len];
            if find_subslice_after_ascii(slice, b"IG_").is_some() {
                if let (Some(vid_idx), Some(pid_idx)) = (
                    find_subslice_after_ascii(slice, b"VID_"),
                    find_subslice_after_ascii(slice, b"PID_"),
                ) {
                    if let (Some(vid), Some(pid)) = (
                        parse_hex4_u16(&slice[vid_idx..]),
                        parse_hex4_u16(&slice[pid_idx..]),
                    ) {
                        result = ((pid as u32) << 16) | (vid as u32);
                        break;
                    }
                }
            }
        }
    }

    SetupDiDestroyDeviceInfoList(dev_info);
    result
}

// ============================================================================
// DirectInput8 EnumDevices Hooks
// ============================================================================

/// State tracked across a single `IDirectInput8A::EnumDevices` invocation.
struct EnumContextA {
    user_cb: DIEnumDevicesCallbackA,
    user_ref: *mut c_void,
    found_gamepad: bool,
    stop: bool,
}

/// State tracked across a single `IDirectInput8W::EnumDevices` invocation.
struct EnumContextW {
    user_cb: DIEnumDevicesCallbackW,
    user_ref: *mut c_void,
    found_gamepad: bool,
    stop: bool,
}

/// Wrapper callback for `IDirectInput8A::EnumDevices`.
/// Observes whether Windows already enumerated a gamepad so we never duplicate devices.
unsafe extern "system" fn wrapper_cb_a(lpddi: *const DIDEVICEINSTANCEA, pvRef: *mut c_void) -> i32 {
    let ctx = &mut *(pvRef as *mut EnumContextA);
    if !lpddi.is_null() {
        let dev_type = (*lpddi).dwDevType & 0xFF;
        // 0x15 = DI8DEVTYPE_GAMEPAD, 0x14 = DI8DEVTYPE_JOYSTICK
        if dev_type == 0x15 || dev_type == 0x14 {
            ctx.found_gamepad = true;
        }
    }
    let res = (ctx.user_cb)(lpddi, ctx.user_ref);
    if res == 0 {
        ctx.stop = true;
    }
    res
}

/// Wrapper callback for `IDirectInput8W::EnumDevices`.
/// Observes whether Windows already enumerated a gamepad so we never duplicate devices.
unsafe extern "system" fn wrapper_cb_w(lpddi: *const DIDEVICEINSTANCEW, pvRef: *mut c_void) -> i32 {
    let ctx = &mut *(pvRef as *mut EnumContextW);
    if !lpddi.is_null() {
        let dev_type = (*lpddi).dwDevType & 0xFF;
        if dev_type == 0x15 || dev_type == 0x14 {
            ctx.found_gamepad = true;
        }
    }
    let res = (ctx.user_cb)(lpddi, ctx.user_ref);
    if res == 0 {
        ctx.stop = true;
    }
    res
}

/// Hooked implementation of `IDirectInput8A::EnumDevices` (vtable slot 4).
unsafe extern "system" fn hooked_enum_devices_a(
    pThis: *mut c_void,
    dwDevType: u32,
    lpCallback: DIEnumDevicesCallbackA,
    pvRef: *mut c_void,
    dwFlags: u32,
) -> i32 {
    let orig_ptr = ORIG_ENUM_A.load(Ordering::Acquire);
    if orig_ptr.is_null() {
        return 0;
    }
    let orig: EnumDevicesAFn = std::mem::transmute(orig_ptr);
    let mut ctx = EnumContextA {
        user_cb: lpCallback,
        user_ref: pvRef,
        found_gamepad: false,
        stop: false,
    };

    // Step 1: Run the real Windows DirectInput8 enumeration.
    let hr = orig(
        pThis,
        dwDevType,
        wrapper_cb_a,
        &mut ctx as *mut _ as *mut c_void,
        dwFlags,
    );

    // Step 2: If the caller requested game controllers and DirectInput found none
    // (because the HID-compliant game controller node is disabled/hidden), check XInput.
    let wants_game_ctrl = dwDevType == 0 || dwDevType == 4 || (dwDevType & 0xFF) == 0x15;
    if hr >= 0 && wants_game_ctrl && !ctx.found_gamepad && !ctx.stop {
        if let Some(xinput_get_caps) = get_xinput_caps_fn() {
            let mut vid_pid: u32 = 0;
            for slot in 0..4u32 {
                let mut caps: XINPUT_CAPABILITIES = std::mem::zeroed();
                if xinput_get_caps(slot, 0, &mut caps) == 0 {
                    if vid_pid == 0 {
                        vid_pid = discover_xinput_vid_pid();
                    }

                    let mut inst: DIDEVICEINSTANCEA = std::mem::zeroed();
                    inst.dwSize = std::mem::size_of::<DIDEVICEINSTANCEA>() as u32;
                    inst.guidInstance = GUID {
                        Data1: vid_pid,
                        Data2: slot as u16,
                        Data3: 0x0000,
                        Data4: [0x00, 0x00, b'P', b'I', b'D', b'V', b'I', b'D'],
                    };
                    inst.guidProduct = GUID {
                        Data1: vid_pid,
                        Data2: 0x0000,
                        Data3: 0x0000,
                        Data4: [0x00, 0x00, b'P', b'I', b'D', b'V', b'I', b'D'],
                    };
                    inst.dwDevType = GAMEPAD_DEV_TYPE;

                    let name = b"Controller (Xbox 360 For Windows)\0";
                    inst.tszInstanceName[..name.len()].copy_from_slice(name);
                    inst.tszProductName[..name.len()].copy_from_slice(name);
                    inst.wUsagePage = 0x0001;
                    inst.wUsage = 0x0005;

                    let res = lpCallback(&inst, pvRef);
                    if res == 0 {
                        break;
                    }
                }
            }
        }
    }
    hr
}

/// Hooked implementation of `IDirectInput8W::EnumDevices` (vtable slot 4).
unsafe extern "system" fn hooked_enum_devices_w(
    pThis: *mut c_void,
    dwDevType: u32,
    lpCallback: DIEnumDevicesCallbackW,
    pvRef: *mut c_void,
    dwFlags: u32,
) -> i32 {
    let orig_ptr = ORIG_ENUM_W.load(Ordering::Acquire);
    if orig_ptr.is_null() {
        return 0;
    }
    let orig: EnumDevicesWFn = std::mem::transmute(orig_ptr);
    let mut ctx = EnumContextW {
        user_cb: lpCallback,
        user_ref: pvRef,
        found_gamepad: false,
        stop: false,
    };

    let hr = orig(
        pThis,
        dwDevType,
        wrapper_cb_w,
        &mut ctx as *mut _ as *mut c_void,
        dwFlags,
    );

    let wants_game_ctrl = dwDevType == 0 || dwDevType == 4 || (dwDevType & 0xFF) == 0x15;
    if hr >= 0 && wants_game_ctrl && !ctx.found_gamepad && !ctx.stop {
        if let Some(xinput_get_caps) = get_xinput_caps_fn() {
            let mut vid_pid: u32 = 0;
            for slot in 0..4u32 {
                let mut caps: XINPUT_CAPABILITIES = std::mem::zeroed();
                if xinput_get_caps(slot, 0, &mut caps) == 0 {
                    if vid_pid == 0 {
                        vid_pid = discover_xinput_vid_pid();
                    }

                    let mut inst: DIDEVICEINSTANCEW = std::mem::zeroed();
                    inst.dwSize = std::mem::size_of::<DIDEVICEINSTANCEW>() as u32;
                    inst.guidInstance = GUID {
                        Data1: vid_pid,
                        Data2: slot as u16,
                        Data3: 0x0000,
                        Data4: [0x00, 0x00, b'P', b'I', b'D', b'V', b'I', b'D'],
                    };
                    inst.guidProduct = GUID {
                        Data1: vid_pid,
                        Data2: 0x0000,
                        Data3: 0x0000,
                        Data4: [0x00, 0x00, b'P', b'I', b'D', b'V', b'I', b'D'],
                    };
                    inst.dwDevType = GAMEPAD_DEV_TYPE;

                    let name = b"Controller (Xbox 360 For Windows)\0";
                    for (i, &b) in name.iter().enumerate() {
                        inst.tszInstanceName[i] = b as u16;
                        inst.tszProductName[i] = b as u16;
                    }
                    inst.wUsagePage = 0x0001;
                    inst.wUsage = 0x0005;

                    let res = lpCallback(&inst, pvRef);
                    if res == 0 {
                        break;
                    }
                }
            }
        }
    }
    hr
}

// ============================================================================
// Exported dinput8.dll Entry Points
// ============================================================================

/// Proxies `DirectInput8Create` to `C:\Windows\System32\dinput8.dll` and installs
/// the `EnumDevices` hook on the returned `IDirectInput8A` or `IDirectInput8W` interface.
#[no_mangle]
pub unsafe extern "system" fn DirectInput8Create(
    hinst: *mut c_void,
    dwVersion: u32,
    riidltf: *const GUID,
    ppvOut: *mut *mut c_void,
    punkOuter: *mut c_void,
) -> i32 {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return -2147467259i32; // E_FAIL
    }
    let proc = GetProcAddress(hmod, b"DirectInput8Create\0".as_ptr());
    if proc.is_null() {
        return -2147467259i32;
    }

    let real_fn: DirectInput8CreateFn = std::mem::transmute(proc);
    let hr = real_fn(hinst, dwVersion, riidltf, ppvOut, punkOuter);

    if hr >= 0 && !ppvOut.is_null() && !(*ppvOut).is_null() && !riidltf.is_null() {
        let obj = *ppvOut as *mut *mut *mut c_void;
        let vtable = *obj;
        let slot4 = vtable.add(4); // Slot 4 is IDirectInput8::EnumDevices
        let mut old_prot = 0u32;
        if VirtualProtect(
            slot4 as *mut c_void,
            std::mem::size_of::<usize>(),
            PAGE_EXECUTE_READWRITE,
            &mut old_prot,
        ) != 0
        {
            let cur_fn = *slot4;
            if *riidltf == IID_IDIRECTINPUT8A {
                if cur_fn != hooked_enum_devices_a as *mut c_void {
                    ORIG_ENUM_A.store(cur_fn, Ordering::Release);
                    *slot4 = hooked_enum_devices_a as *mut c_void;
                }
            } else if *riidltf == IID_IDIRECTINPUT8W {
                if cur_fn != hooked_enum_devices_w as *mut c_void {
                    ORIG_ENUM_W.store(cur_fn, Ordering::Release);
                    *slot4 = hooked_enum_devices_w as *mut c_void;
                }
            }
            let mut dummy = 0u32;
            VirtualProtect(
                slot4 as *mut c_void,
                std::mem::size_of::<usize>(),
                old_prot,
                &mut dummy,
            );
        }
    }
    hr
}

/// Forwards `DllCanUnloadNow` to `C:\Windows\System32\dinput8.dll`.
#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> i32 {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return 0;
    }
    let proc = GetProcAddress(hmod, b"DllCanUnloadNow\0".as_ptr());
    if proc.is_null() {
        return 0;
    }
    let f: DllCanUnloadNowFn = std::mem::transmute(proc);
    f()
}

/// Forwards `DllGetClassObject` to `C:\Windows\System32\dinput8.dll`.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return -2147467259i32;
    }
    let proc = GetProcAddress(hmod, b"DllGetClassObject\0".as_ptr());
    if proc.is_null() {
        return -2147467259i32;
    }
    let f: DllGetClassObjectFn = std::mem::transmute(proc);
    f(rclsid, riid, ppv)
}

/// Forwards `DllRegisterServer` to `C:\Windows\System32\dinput8.dll`.
#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> i32 {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return 0;
    }
    let proc = GetProcAddress(hmod, b"DllRegisterServer\0".as_ptr());
    if proc.is_null() {
        return 0;
    }
    let f: DllRegisterServerFn = std::mem::transmute(proc);
    f()
}

/// Forwards `DllUnregisterServer` to `C:\Windows\System32\dinput8.dll`.
#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> i32 {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return 0;
    }
    let proc = GetProcAddress(hmod, b"DllUnregisterServer\0".as_ptr());
    if proc.is_null() {
        return 0;
    }
    let f: DllUnregisterServerFn = std::mem::transmute(proc);
    f()
}

/// Forwards `GetdfDIJoystick` to `C:\Windows\System32\dinput8.dll`.
#[no_mangle]
pub unsafe extern "system" fn GetdfDIJoystick() -> *const c_void {
    let hmod = get_real_dinput8();
    if hmod.is_null() {
        return ptr::null();
    }
    let proc = GetProcAddress(hmod, b"GetdfDIJoystick\0".as_ptr());
    if proc.is_null() {
        return ptr::null();
    }
    let f: GetdfDIJoystickFn = std::mem::transmute(proc);
    f()
}
