# MacFan

Fan control and sensor monitor written in Rust for Intel Macs running Windows (Boot Camp). It works the same way as Macs Fan Control.

## Features
- Live RPM for every fan (current / min / max)
- Three control modes per fan:
  - **Auto**: the SMC controls the fan
  - **Constant RPM**: a fixed speed
  - **Sensor-based**: pick a sensor and two temperatures; the fan ramps linearly between min and max RPM
- Temperature, power (W), voltage and current sensors read from all SMC keys
- **Full Blast**: one click runs every fan at maximum RPM; click again to return to your per-fan settings. You can also toggle it from the tray menu.
- **Minimize to tray**: minimizing (or the "Hide to tray" button) sends the window to the system tray while fan control keeps running in the background. Left-click the tray icon to bring it back. The tray menu has Show, Full Blast and Quit (Quit returns the fans to Auto). The tooltip shows the fan RPMs and CPU temperature.
- °C / °F, refresh interval, option to show unnamed sensors, minimize-to-tray on/off
- Settings are saved to `%APPDATA%\MacFan\config.json`

## Safety
Like Macs Fan Control, the app raises the fan's **minimum speed** (`FnMn`) instead of taking the fan away from the SMC. The SMC can still speed the fan up whenever it needs to.
- Closing the app returns every fan to Auto.
- If the app crashes, the original minimums are restored from the config file on the next start. A reboot also resets the SMC.
- In sensor-based mode, if the chosen sensor stops reporting, the fan goes to maximum speed.

### T2 Macs (2018–2020, e.g. MacBookPro16,1)
- The SMC is reached only over MMIO. Its address is read from the ACPI resources of the `APP0001` device (on this machine `0xFE0B0000`).
- `FnMn` (minimum speed) is read-only on T2 (SMC error 0x86). The app switches to **forced mode** automatically: it sets `FnMd=1` and writes the RPM to `FnTg`.
- Forced mode takes the fan away from the SMC, so there are two extra safeguards:
  - **Overheat guard:** while any CPU/GPU sensor is at 90 °C or above, every custom-controlled fan runs at maximum, until the temperature falls below 85 °C.
  - **Release on start:** `FnMd=0` is written to every fan at startup, so a forced fan left behind by a crash is handed back to the SMC.
- Choosing Auto or closing the app writes `FnMd=0`.

## Building
With the MSVC toolchain, `cargo build --release` is all you need. The GNU toolchain (`x86_64-pc-windows-gnu`) also needs MinGW binutils (`dlltool`, `as`) on PATH:
```powershell
$env:PATH = "C:\path\to\mingw64\bin;" + $env:PATH
cargo build --release
copy driver\inpoutx64.dll target\release\
```

`driver\inpoutx64.dll` is InpOut32/64 by Phil Gibbons (highrez.co.uk), included under its MIT license (`driver\license.txt`).

## Running
- `target\release\macfan.exe` asks for administrator rights; the driver needs them.
- `macfan.exe --demo`: runs on simulated SMC data, no driver needed.
- `macfan.exe --dump`: writes every SMC key to `smc-dump.txt` (for diagnostics).
- `macfan.exe --no-mmio`: forces the port I/O path on non-T2 Macs.
- `macfan.exe --selftest`: SMC write test (tries Fan 1's minimum speed, then forced mode at max speed, and restores both); writes `selftest.txt`.
- Building with `MACFAN_NO_ADMIN=1` produces an exe that doesn't ask for admin (for `--demo` testing).

## Hardware access
| Driver | Port I/O | MMIO (T2) |
|---|---|---|
| inpoutx64.dll (recommended) | ✓ | ✓ |
| WinRing0x64.dll + .sys | ✓ | ✗ (Defender may block it) |

The SMC is reached through I/O ports 0x300/0x304, or on T2 Macs through MMIO (the address comes from ACPI). The protocol follows the Linux `applesmc` driver and the t2linux patches.
