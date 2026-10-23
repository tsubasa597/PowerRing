# PowerRing

> 中文版：[README.md](README.md)

Battery monitor for wireless peripherals on Windows. A ring in the tray tells you how much juice your mouse and headset have left — without installing one vendor app per brand.

- **Single ~0.9 MB exe**, portable, no admin rights needed
- **No network, no account, no telemetry** — everything stays on your machine
- **Adding a device = adding a YAML file**, no code changes (see [Adding a device](#adding-a-device))

## Features

| Feature | Notes |
| --- | --- |
| Tray battery ring | Ring fill maps to remaining charge. Green → yellow → red as it drains, blue while charging. **Unreadable is shown as a grey ring — never as 0%** |
| Hover readout | Shows percentage per device. With multiple devices it uses short names, sorted lowest first |
| Left click | Toggles the battery panel with every device's full name and level |
| Right click menu | Show panel / Refresh now / Settings… / Exit |
| Low battery alerts | Fires once when crossing a threshold, then stays quiet. Card is anchored next to the tray icon |
| Start with Windows | Writes `HKCU\...\Run`, no elevation. A named mutex keeps a second instance from spawning a second icon |
| Theme | Follow system / light / dark. You can force dark even when apps are light and only the taskbar is dark |
| Auto discovery | Rescans the bus every 60 s: a device you just switched on shows up, one you switched off stops reporting stale numbers |
| Logs | `%APPDATA%\PowerRing\log.txt`, rotates above 1 MB |

Settings expose the refresh interval, warning and critical thresholds, the alert toggle (with a "Test once" button so you can verify it immediately), start-with-Windows and the theme.

## Verified devices

| Device | Interface | Reading taken from | Status |
| --- | --- | --- | --- |
| Logitech G PRO Wireless | receiver `046D:C539`, usage page `FF00` / usage `0002`, slots 1–6 | HID++ 2.0 `0x1001` voltage (mV → discharge curve) | Verified. 3744 mV reads as 28%, G HUB showed 29% at the same time |
| Logitech G PRO Wireless (plugged in) | **USB cable** `046D:C088`, usage page `FF00` / usage `0002` | same `0x1001` feature | Verified. 4101 mV reads as 93%, correctly shown as charging |
| HyperX Cloud Flight | receiver `0951:1723`, usage page `FF90` / usage `0303` | vendor report `21 FF 05` (direct mode) | Verified. Raw value 231 reads as 60% |
| Xbox Wireless Controller (Series) | **Bluetooth**, VID `045E` / PID `0B13` | standard Battery Service `0x180F` / `0x2A19` | Verified. Device reports 17% itself |

The third row needs **no reverse engineering at all**: the Bluetooth SIG standardised how to read battery level, so any compliant device is read by UUID and its YAML carries not a single byte of a report. To list every such device on your machine:

```bash
cargo run --release --example ble_probe
```

Device declarations live in `devices/`. Every YAML carries `protocol_source` (where the constants came from) and `verification`. **Anything not tested on real hardware is marked `unverified`.**

## Usage

Download the zip from Releases, extract it anywhere (e.g. `C:\Tools\PowerRing`) and double-click `powerring.exe`. The exe must sit next to the `devices` folder:

```
PowerRing/
├─ powerring.exe
└─ devices/
   ├─ logitech_gpw.yaml
   └─ hyperx_cloud_flight.yaml
```

> Windows 10 hides new tray icons inside the `^` overflow. If you don't see it, go to Taskbar settings → Notification area → Select which icons appear on the taskbar.

## Adding a device

Copy the closest YAML in `devices/` and edit a few lines. **No Rust code changes required.** Three flavours:

**Bluetooth standard devices** (controllers, some keyboards) need nothing but VID/PID — no report bytes at all:

```yaml
protocol: "ble"
transport: "direct"
match:
  vendor_id: "0x045E"
  product_id: "0x0B13"   # product id in Bluetooth mode
battery:
  kind: "percent"        # Battery Level already is a percentage
```

**HID++ 2.0 devices** (Logitech family) use a feature index plus a reading mode:

```yaml
name: "Logitech G PRO Wireless"
short: "GPW"              # used in hover text; falls back to full name
protocol: "hid++2.0"
transport: "receiver"     # address via receiver slots; use "direct" for direct connection

match:                    # which HID interface to open
  vendor_id: "0x046D"
  product_id: "0xC539"
  usage_page: "0xFF00"
  usage: "0x0002"

slots: [1, 2, 3, 4, 5, 6] # receiver slots to probe, defaults to 1..6
battery:
  feature: "0x1001"       # battery feature index
  kind: "voltage"         # voltage = read mV then convert; percent = already a percentage
identify:                 # a receiver may carry other devices, this double-checks it
  name_feature: "0x0005"
  contains: "PRO WIRELESS"
protocol_source: "Logitech HID++ 2.0, constants referenced from Solaar (GPL-3.0) public docs"
verification: "verified"
```

**Everything else** uses `raw`: what to send, which bytes to read and how to convert are all declared. This is the real HyperX Cloud Flight config — it landed without touching a single line of Rust:

```yaml
protocol: "raw"
transport: "direct"
match:
  vendor_id: "0x0951"
  product_id: "0x1723"
  usage_page: "0xFF90"
  usage: "0x0303"
battery:
  request: "21 FF 05"     # request bytes, first byte is the report id
  reply_len: 20           # many devices are picky about length and stay silent otherwise
  state_index: 3          # index of the "level band" byte in the reply
  value_index: 4          # index of the value byte in the reply
  charging_states: ["0x10", "0x11"]
  table:                  # piecewise mapping: which range maps to which percentage
    "0x0e":
      - [89, 10]
      - [239, 60]
      - [255, 65]
    "0x0f":
      - [19, 70]
      - [129, 95]
      - [255, 100]
```

A lookup table rather than a formula: on these devices the raw value is **not linear** against percentage. A linear fit drifts by more than ten points in the upper range.

Full field reference and a step-by-step sniffing walkthrough live in [docs/adding-a-device.md](docs/adding-a-device.md). If you want your device supported, open an issue with the "New device request" template.

## Building

Needs a Rust toolchain (Windows defaults to `x86_64-pc-windows-msvc`, which requires VS 2022 build tools).

```bash
cargo build --release
```

Output: `target/release/powerring.exe` (~0.9 MB).

## Protocol notes worth knowing

**Logitech G PRO Wireless** (HID++ 2.0)

- No `0x1004` unified battery support; `0x1001` voltage reporting only
- Long reports must be exactly **20 bytes**, short ones **7 bytes**; a wrong length gets no reply or an `0x8F` error
- Probe receiver slots with `getFeature`, **not** ping
- Voltage jitters by about 1 mV — the app applies hysteresis and ignores sub-2-point changes
- Device names inside HID reports are NUL-padded; `trim()` does not strip those NULs

**HyperX Cloud Flight**

- The receiver exposes 4 interfaces and **only `FF90:0303` answers** battery requests. The other three (`000C:0001` / `FF00:0001` / `FFC0:0001`) stay silent — probe more than one
- With the headset powered off there is no reply, and the app correctly excludes it instead of showing 0%
- Its report id is `0x21`, higher than the usual `0x00`–`0x10` sweep, so it is easy to miss

## Known limitations

- **Bluetooth devices are supported, with a caveat**: they must expose the standard Battery Service (controllers, some keyboards). Anything on a vendor-proprietary protocol such as AirPods is out — Windows already surfaces those anyway
- Xbox controllers are readable **only over Bluetooth**: USB cable never routes through GATT and the wireless adapter speaks Microsoft's proprietary GIP
- Bluetooth Battery Level carries a single percentage byte with **no charging flag**, so these devices never show as charging
- **Bluetooth readings are the last value Windows synced, not a live one**: they come through the WinRT GATT cache and Windows decides when to refresh (a few minutes apart in practice). The upside is that it is literally the number the Bluetooth settings page shows, so two apps never disagree
- Following from that: **paired devices that are no longer around keep being enumerated and keep reporting their last known value**, same for powered-off devices. They stay in the list showing a stale percentage. That is the cost of riding on the system's state — noticeable if you have a drawer full of once-paired peripherals
- When a reading fails the ring turns grey instead of guessing

## License boundary

Protocol constants and report layouts are facts and may be carried over; **do not copy source code** from GPL projects like Solaar, HeadsetControl or OpenRazer. Every device declaration must fill in `protocol_source`. When citing a GPL project, take constants only — never code.

PowerRing itself is **MIT**, see [LICENSE](LICENSE).
