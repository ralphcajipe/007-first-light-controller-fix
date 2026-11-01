# 007 First Light - PC Controller Detection Fix

A tiny, plug-and-play fix for **007 First Light** on PC when the game fails to detect your **8BitDo**, **Xbox**, or other **XInput** controller and locks your settings to Mouse and Keyboard.

> **Curious how it works under the hood?** Read [TECHNICAL_NOTES.md](TECHNICAL_NOTES.md) for the developer breakdown.

---

## Quick Installation (30 Seconds)

1. Download **`dinput8.dll`** from the [Releases](../../releases) page (or build it yourself below).
2. Copy `dinput8.dll` into your game's **`Retail`** folder (the same folder that contains `007FirstLight.exe`):
   ```text
   007 First Light\
   └── Retail\
       ├── 007FirstLight.exe
       └── dinput8.dll        <-- Paste here
   ```
3. Launch the game. Your controller will now be detected automatically.

* **To uninstall:** Simply delete `dinput8.dll` from the `Retail` folder.

---

## 1. The Problem (In Plain English)

When you plug an Xbox-style controller (like an 8BitDo Wired Controller, Xbox Series/One controller, GameSir, or GuliKit) into a Windows PC, Windows actually registers it as **two** things at the same time:

1. **An Xbox Controller (XInput):** The modern Windows system that reads your buttons, triggers, and thumbsticks.
2. **A "HID-compliant game controller" (DirectInput):** An older Windows label used to list plugged-in controllers.

On many gaming PCs, part #2 (**HID-compliant game controller**) gets turned **OFF (Disabled)** in Windows Device Manager or hidden by tools like **HidHide** or **DS4Windows** (often to stop "double input" bugs in other games).

* **Most games** only check part #1, so your controller works fine in them.
* **007 First Light** checks part #2 **first** just to see if a controller is plugged in before it ever checks part #1. Because part #2 is disabled or hidden, the game thinks **zero controllers** are plugged in and locks you to Mouse and Keyboard.

---

## 2. The Solution

Instead of forcing you to change your Windows Device Manager settings or break your HidHide setup for other games, this project gives `007 First Light` a small translator file (`dinput8.dll`).

When the game asks Windows *"Are any controllers plugged in?"*, this file checks if your Xbox/8BitDo controller is active and answers: *"Yes, a controller is plugged in!"* The game then switches to normal Xbox (XInput) mode and reads all your buttons, sticks, and vibration natively.

---

## 3. Does This Affect Other Games?

**No.** This fix is 100% isolated to `007 First Light`:
* `dinput8.dll` stays inside the `007 First Light\Retail` folder and only runs while `007FirstLight.exe` is open.
* No Windows system files, drivers, or registry settings are changed.
* **No double-input issues:** If Windows ever detects your controller on its own, `dinput8.dll` automatically steps aside and does nothing so you never get duplicate inputs.

---

## 4. Results

* **Before:** The game reports `0` controllers found and cannot switch away from Mouse and Keyboard settings.
* **After:** The game detects your controller immediately at startup and unlocks all controller settings and button prompts.

---

## 5. Alternative Fix (Without Using Any Files)

If you do not use HidHide and just want to turn the disabled Windows setting back on in **Device Manager**:

1. Open the Start Menu, type **Device Manager**, and press Enter.
2. Expand **Human Interface Devices**.
3. Look for **HID-compliant game controller** with a small **down-arrow icon** on it.
4. Right-click it and select **Enable device**.

*(Or run this command in an **Administrator PowerShell** window:)*
```powershell
Get-PnpDevice | Where-Object { $_.ConfigManagerErrorCode -eq 22 -and $_.FriendlyName -eq 'HID-compliant game controller' } | Enable-PnpDevice -Confirm:$false
```

---

## 6. Building From Source

This project is written in safe, dependency-free Rust (`src/lib.rs`) using standard Windows APIs. To compile `dinput8.dll` yourself:

```powershell
cargo build --release
```

The compiled file will be located at `target\release\dinput8.dll`.

---

## Disclaimer

This software is an independent, unofficial open-source hardware compatibility utility created for interoperability with standard Microsoft Windows DirectInput and XInput APIs. It contains no proprietary game code or assets, does not modify any game files, and is not affiliated with, endorsed by, or connected to IO Interactive, Metro-Goldwyn-Mayer (MGM), or Amazon MGM Studios. All trademarks belong to their respective owners.

## License

Released under the [MIT License](LICENSE).
