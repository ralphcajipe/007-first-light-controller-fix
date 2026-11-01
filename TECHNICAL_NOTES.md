# Technical Notes: Windows DirectInput8 & XInput Interoperability

This document explains the technical reason why games that follow Microsoft's classic DirectX SDK **"XInput and DirectInput"** enumeration pattern fail to detect XInput controllers when their companion `HID-compliant game controller` node is disabled or hidden, and how our `dinput8.dll` proxy resolves it.

---

## 1. Background: Dual-Interface XInput Controllers on Windows

When an XInput-compatible USB or wireless controller (such as an Xbox 360/One/Series controller or an 8BitDo controller in X-Input mode) is connected to Windows, the Windows Xbox composite driver (`xusb22.sys` / `xinputhid.sys`) exposes two distinct interfaces in the Plug and Play (PnP) device tree:

1. **The XUSB / XInput Interface (`USB\VID_xxxx&PID_xxxx&IG_00`):**
   Communicates with `xinput1_4.dll` and `xinput9_1_0.dll` via `XInputGetState`, `XInputSetState`, and `XInputGetCapabilities`.
2. **The Legacy HID Game Controller Node (`HID\VID_xxxx&PID_xxxx&IG_00`):**
   A child `HID-compliant game controller` node (`UsagePage = 0x01`, `Usage = 0x05`) that allows legacy `dinput8.dll` (`IDirectInput8::EnumDevices`) and `joy.cpl` to see the controller.

---

## 2. The Classic DirectX SDK Discovery Pattern (`IsXInputDevice`)

Microsoft's official DirectX SDK documentation (*"XInput and DirectInput"*) introduced a standard algorithm (`IsXInputDevice`) for games that support both DirectInput devices (keyboards, mice, PlayStation controllers, flight sticks) and XInput gamepads:

```text
[Step 1: DirectInput Enumeration]
IDirectInput8::EnumDevices(DI8DEVCLASS_GAMECTRL, Callback, pvRef, DIEDFL_ATTACHEDONLY)
       |
       +-- If 0 HID gamepads are enumerated --> Callback never runs!
       |
       v
[Step 2: Microsoft MSDN IsXInputDevice(guidProduct) Check]
Queries WMI (\\.\root\cimv2 -> Win32_PNPEntity)
  -> Inspects each DeviceID containing "IG_" (e.g., USB\VID_2DC8&PID_310A&IG_00)
  -> Parses 16-bit hexadecimal VID_%4X and PID_%4X
  -> Computes MAKELONG(VID, PID) = (PID << 16) | VID
  -> Compares MAKELONG(VID, PID) against DIDEVICEINSTANCE::guidProduct.Data1
       |
       v
[Step 3: XInput Slot Binding]
Loops XInputGetCapabilities(0..3, 0, &caps)
  -> Binds the controller to the active XInput user index (0..3)
  -> Reads input state via XInputGetState and vibration via XInputSetState
```

### Why This Fails When the HID Node Is Disabled or Hidden

Notice the bottleneck in **Step 1**:
* Even though the game never opens a DirectInput device (`IDirectInput8::CreateDevice`) for an XInput controller and reads all input through `xinput9_1_0.dll`, it relies on `IDirectInput8::EnumDevices(DI8DEVCLASS_GAMECTRL)` as a gatekeeper to learn that a controller is present.
* If the child `HID-compliant game controller` node (`HID\VID_xxxx&PID_xxxx&IG_00`) is disabled in Windows Device Manager (`CM_PROB_DISABLED`, Error Code 22) or hidden by input-isolation software like **HidHide**, `IDirectInput8::EnumDevices(DI8DEVCLASS_GAMECTRL)` returns **0 devices**.
* Meanwhile, the parent USB XInput node (`USB\VID_xxxx&PID_xxxx&IG_00`) remains active in WMI (`Win32_PNPEntity`) and on XInput (`XInputGetCapabilities` returns `ERROR_SUCCESS`).

---

## 3. How the `dinput8.dll` Proxy Solves the Problem

Our `src/lib.rs` compiles into a lightweight 64-bit `dinput8.dll` wrapper that sits alongside the application executable:

### Step A: Transparent COM Forwarding
All standard `dinput8.dll` exports (`DirectInput8Create`, `DllCanUnloadNow`, `DllGetClassObject`, `DllRegisterServer`, `DllUnregisterServer`, `GetdfDIJoystick`) are forwarded to `C:\Windows\System32\dinput8.dll`.

When `DirectInput8Create` returns an `IDirectInput8A` (`{BF798030-483A-4DA2-AA99-5D64ED369700}`) or `IDirectInput8W` (`{BF798031-483A-4DA2-AA99-5D64ED369700}`) interface pointer, our proxy hooks vtable slot 4 (`EnumDevices`).

### Step B: Anti-Duplication Pass-Through
When `EnumDevices` is called:
1. It first invokes the genuine system `IDirectInput8::EnumDevices` with a thin wrapper callback (`wrapper_cb_a` / `wrapper_cb_w`).
2. The wrapper forwards every device untouched to the application's callback and checks whether any gamepad or joystick (`DI8DEVTYPE_GAMEPAD = 0x15` or `DI8DEVTYPE_JOYSTICK = 0x14`) was already enumerated by Windows.
3. If Windows already reported a gamepad, the proxy does nothing further. This guarantees zero duplicate controller entries.

### Step C: Dynamic XInput & SetupAPI Hardware ID Synthesis
If the caller requested game controllers (`DI8DEVCLASS_GAMECTRL = 4` or `DI8DEVCLASS_ALL = 0`) and DirectInput enumerated zero gamepads:
1. The proxy queries `XInputGetCapabilities(slot, 0, &caps)` across slots `0..3`.
2. When an active XInput controller is found (`ERROR_SUCCESS`), the proxy calls `discover_xinput_vid_pid()`:
   * Uses Windows `SetupDiGetClassDevsW(NULL, NULL, NULL, DIGCF_ALLCLASSES | DIGCF_PRESENT)` and `SetupDiGetDeviceInstanceIdW` to find any present PnP device whose Instance ID contains `"IG_"` (for example, `USB\VID_2DC8&PID_310A&IG_00\...`).
   * Extracts the 4-digit hex `VID` and `PID` and packs them into `MAKELONG(VID, PID)` (`(PID << 16) | VID`).
3. The proxy populates a standard `DIDEVICEINSTANCEA` (or `DIDEVICEINSTANCEW`) structure:
   * `dwSize`: `sizeof(DIDEVICEINSTANCEA)` (`580`) or `sizeof(DIDEVICEINSTANCEW)` (`1100`)
   * `guidInstance`: `{ MAKELONG(VID, PID), slot, 0x0000, [0, 0, 'P', 'I', 'D', 'V', 'I', 'D'] }`
   * `guidProduct`: `{ MAKELONG(VID, PID), 0x0000, 0x0000, [0, 0, 'P', 'I', 'D', 'V', 'I', 'D'] }`
   * `dwDevType`: `0x00010215` (`DIDEVTYPE_HID | (DI8DEVTYPEGAMEPAD_STANDARD << 8) | DI8DEVTYPE_GAMEPAD`)
   * `tszInstanceName` / `tszProductName`: `"Controller (Xbox 360 For Windows)"`
   * `wUsagePage`: `0x0001` (`HID_USAGE_PAGE_GENERIC`)
   * `wUsage`: `0x0005` (`HID_USAGE_GENERIC_GAMEPAD`)
4. It invokes the application's `EnumDevices` callback with that synthesized `DIDEVICEINSTANCE`. The application's `IsXInputDevice` WMI check then matches `guidProduct.Data1` against the active `IG_` PnP node and binds the XInput slot.
