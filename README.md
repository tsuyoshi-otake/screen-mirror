# screen-mirror

Low-latency LAN screen mirroring for Windows and Android. This is not Miracast-compatible; it uses a small custom discovery protocol plus RTP/H.264 over UDP.

## Monorepo Layout

- `apps/desktop`: Rust Windows tray app and CLI
- `apps/android`: native Android app project that builds an APK
- `crates/sm-core`: shared Rust protocol definitions
- `installer`: WiX SDK project for the Windows MSI
- `scripts`: local build/check scripts

## Transport Model

- Discovery: UDP broadcast on port `47777`
- Touch/control: UDP JSON on port `47778`
- Diagnostics: TCP JSON request/report response on port `47779`
- Video: RTP/H.264 over UDP on port `5004`
- Audio: optional Opus/RTP over UDP on port `5005`
- Pairing: sender and receiver must use the same four-digit PIN; discovery/control packets carry only a SHA-256 PIN hash
- Sender fan-out: one sender can stream to multiple receivers with `multiudpsink`
- Default auto-connect limit: `3` receivers, so `1:3` is the standard target
- Reconnect: receiver advertisements repeat once per second; desktop auto sender refreshes receiver targets while running

## Desktop App

Run the Windows tray app:

```powershell
cargo run -p screen-mirror --release
```

Tray actions:

- `Start as Receiver`: listens on `:5004` and advertises itself on the LAN
- `Start as Sender`: continuously discovers receivers and streams to up to three
- `Stop`: stops the active pipeline
- `Enable Autostart`: registers the tray app under HKCU Run
- `Check for Updates`: checks GitHub Releases immediately
- `Run Diagnostics`: writes a copyable debug report, copies it to the clipboard, and opens it in Notepad
- `Run Peer Diagnostics`: discovers a sender with the same PIN, requests its debug report, copies it to the clipboard, and opens it in Notepad
- `Install/Repair Virtual Display Driver`: installs the bundled VDD driver without creating duplicates when it already exists
- `Show/Enable/Disable/Remove All Bundled Virtual Displays`: manages bundled MTT VDD display devices and monitors. Install/Enable does not request Windows extended desktop; sender mode requests it only after a matching receiver is found.
- `Open Display Settings`: opens Windows display settings
- `Open Config`: opens `%APPDATA%\screen-mirror\config.toml`

## Quick Start

### Windows to Android as an extended display

1. Install `ScreenMirror.msi` from the latest GitHub Release.
2. Start `Screen Mirror` from the Start Menu or the system tray.
3. Open the tray menu and run `Install/Repair Virtual Display Driver`.
4. Allow the UAC prompt, then confirm Windows Display Settings shows an extra display.
5. Set the same four-digit PIN on Windows and Android. The default is `0000`.
6. Start the Android app and tap `Start Receiver`.
7. On Windows, choose `Start as Sender` from the tray menu.
8. If needed, run `screen-mirror.exe monitors` and set `[send].monitor_index` in `%APPDATA%\screen-mirror\config.toml`.

### Windows to Windows

1. Install the MSI on both machines.
2. Set the same four-digit PIN in `%APPDATA%\screen-mirror\config.toml` on both machines. The default is `0000`.
3. On the target machine, choose `Start as Receiver`.
4. On the source machine, choose `Start as Sender`.
5. Keep `host = "auto"` for LAN discovery, or set an explicit receiver IP in `[send].host`.

### Manual CLI run

```powershell
screen-mirror.exe recv --port 5004 --pin 1234
screen-mirror.exe send --host auto --port 5004 --pin 1234
```

Default sender config uses:

```toml
[security]
pin = "0000"

[send]
host = "auto"
port = 5004
audio_enabled = false
audio_port = 5005
audio_bitrate = 96000
audio_frame_ms = "5"
max_receivers = 3
prefer_virtual_display = true
enable_virtual_display = true
sync_virtual_display_resolution = true
monitor_index = -1
fps = 60
bitrate = 12000
mtu = 1200
udp_buffer_size = 4194304
qos_dscp = -1
allow_software_encoder = false
nvidia_tuning = "auto"

[recv]
audio_enabled = false
audio_port = 5005
audio_jitter_ms = 15
jitter_ms = 15
udp_buffer_size = 4194304
mtu = 1200
jitter_faststart_packets = 2
jitter_max_dropout_ms = 200
jitter_max_misorder_ms = 50
fullscreen = true
```

## PIN Pairing

- PIN values must be exactly four numeric digits.
- Windows stores the PIN in `%APPDATA%\screen-mirror\config.toml` under `[security].pin`.
- The tray app reloads `%APPDATA%\screen-mirror\config.toml` before starting sender or receiver mode, so a changed PIN is applied on the next start.
- Android stores the PIN from the app input field and reuses it on the next launch.
- Auto discovery ignores receivers with a different PIN hash, so mismatched devices do not auto-connect.
- This is pairing protection for trusted LAN use, not strong encryption. RTP/H.264 video is still sent over plain UDP.

## Virtual Display Mode

For a SuperDisplay-like extended desktop workflow, screen-mirror uses Virtual Display Driver (VDD) as the Windows virtual monitor and streams that display.

The MSI bundles the signed VDD Driver Only package and `devcon.exe` from the official VDD Control release under the install directory. Use the tray menu item `Install/Repair Virtual Display Driver` to run an idempotent install:

```powershell
devcon.exe install "vdd\MttVDD.inf" Root\MttVDD
```

This launches the driver install through UAC and creates the root-enumerated MTT VDD display device only if one does not already exist. Depending on Windows/driver state, the instance can appear as `ROOT\DISPLAY\...` with an attached `DISPLAY\MTT1337\...` monitor. `pnputil /add-driver` alone is not enough because it only stages/updates matching devices. If driver installation is blocked by policy or times out, install/update VDD manually from <https://github.com/VirtualDrivers/Virtual-Display-Driver/releases> or run:

```powershell
winget install --id=VirtualDrivers.Virtual-Display-Driver -e
```

Runtime behavior:

1. Start the receiver on the tablet/second PC.
2. Receiver discovery advertises its display resolution.
3. Start the desktop sender.
4. With `host = "auto"`, the sender waits until at least one receiver with a matching PIN is discovered.
5. After a matching receiver is found, the sender ensures the bundled MTT VDD device exists. It requests Windows extended-display mode only when that display is not already capture-ready.
6. Sender mode prefers the bundled MTT VDD display for capture. Other virtual displays are fallback candidates, but SuperDisplay is not auto-selected for Screen Mirror capture.
7. If the bundled MTT VDD virtual monitor is visible, the sender tries to match its resolution to the first receiver.
8. When auto sender mode loses every matching receiver beyond the disconnect grace period, it requests bundled VDD removal once so the virtual display is not left in Windows while disconnected.
9. Repeated receiver discovery does not rerun PowerShell, VDD removal, or `DisplaySwitch.exe`; those operations run only when the receiver set changes.
10. With `prefer_virtual_display = true` and `monitor_index = -1`, the sender captures that virtual monitor and falls back to the primary monitor if none is found.

Use the tray menu to show, enable, disable, or remove all bundled MTT VDD devices and monitors. If repeated installs created two or more virtual displays, the remove action deletes every bundled MTT VDD display/monitor after confirmation.
Automatic VDD lifecycle commands are limited to connection state changes. Use the tray management actions for explicit repair, disable, or removal operations.

List capture indexes:

```powershell
screen-mirror.exe monitors
```

Force a specific display or disable VDD automation:

```toml
[send]
prefer_virtual_display = false
enable_virtual_display = false
sync_virtual_display_resolution = false
monitor_index = 1
```

The tray menu includes `Open Virtual Display Driver Page` for manual repair/update.

## GPU Encoding

The sender uses D3D11 screen capture and prefers hardware encoders:

1. `nvd3d11h264enc` for NVIDIA GeForce GTX/RTX
2. `mfh264enc` for Windows Media Foundation hardware encoders
3. `qsvh264enc` for Intel Quick Sync

By default `allow_software_encoder = false`, so `auto` will not silently fall back to CPU `x264enc`. Set it to `true` only if software fallback is acceptable.

NVIDIA tuning:

```toml
[send]
nvidia_tuning = "auto"        # auto-detects GTX/RTX where possible
# nvidia_tuning = "gtx"       # strict low-latency NVENC path
# nvidia_tuning = "rtx"       # low-latency path plus NVENC AQ
# nvidia_tuning = "low-latency"
```

Explicit multi-target sending also works:

```powershell
cargo run -p screen-mirror --release -- send --host 192.168.1.20,192.168.1.21,192.168.1.22 --port 5004
```

Per-target ports are supported:

```powershell
cargo run -p screen-mirror --release -- send --host 192.168.1.20:5004,192.168.1.21:5005
```

Discover receivers:

```powershell
cargo run -p screen-mirror -- discover
```

## Windows Requirements

- Rust toolchain
- .NET SDK 6+ for MSI builds
- GStreamer MSVC x86_64 runtime + development files
- GStreamer Bad/Libav/Ugly plugins

Example GStreamer environment:

```powershell
$env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\gstreamer\1.0\msvc_x86_64"
$env:Path = "$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\bin;$env:Path"
```

## MSI

Build the MSI:

```powershell
.\scripts\build-msi.ps1
```

The MSI installs `screen-mirror.exe`, the tray/start-menu icon, autostart registration, and the required GStreamer runtime DLL/plugin files.
The tray app is launched after install/update and is registered under HKCU Run for the installing Windows user.

## Diagnostics

Use tray menu `Run Diagnostics` to collect a local debug report. The report is saved under `%TEMP%`, opened in Notepad, and copied to the clipboard so it can be pasted into an issue or chat.

Use tray menu `Run Peer Diagnostics` on a receiver to collect diagnostics from a sender with the same PIN. The sender advertises a diagnostics endpoint while sender mode is active, and the receiver requests the report over TCP `47779`. The received report is saved under `%TEMP%`, opened in Notepad, and copied to the receiver clipboard.

The report includes:

- Raw `%APPDATA%\screen-mirror\config.toml`, including the four-digit PIN
- Recent Screen Mirror log lines
- Installed version, autostart entry, and running process list
- Bundled VDD device status and other virtual display candidates
- Windows display list, GStreamer probe, receiver discovery, UDP endpoints, network adapters, and related firewall rule summaries

## Auto Update

The tray app checks GitHub Releases automatically:

- First check: 30 seconds after tray startup
- Regular interval: once per hour
- Manual check: tray menu `Check for Updates`
- Asset name: `ScreenMirror.msi`
- Version lookup and download run inside the Screen Mirror process over HTTPS; periodic checks do not launch `curl.exe`, PowerShell, or `cmd.exe`
- Install mode: hidden direct `msiexec.exe /i <msi> /qn /norestart`; no `cmd.exe` wrapper window

## Receiver Power Behavior

Receiver mode prevents the display and system from sleeping while video reception is active:

- Windows receiver calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)`.
- Android receiver sets `FLAG_KEEP_SCREEN_ON` and `SurfaceView.setKeepScreenOn(true)`.
- The sleep/display guard is released when receiver mode stops.
- Windows receiver mode exits back to idle after video packets stop arriving, so a fullscreen receiver window is not left behind after sender disconnect.

## Receiver Window

The Windows receiver uses GStreamer `d3d11videosink` for GPU rendering, but screen-mirror renames the generated render window while receiver mode is active:

- Taskbar/window title: `screen-mirror Receiver`
- Window icon: the same simple display icon used by the app and tray
- Fullscreen: enabled by default with `[recv].fullscreen = true`

## Audio

Audio transfer is optional and uses a separate low-latency Opus/RTP stream:

```toml
[send]
audio_enabled = true
audio_port = 5005
audio_bitrate = 96000
audio_frame_ms = "5"

[recv]
audio_enabled = true
audio_port = 5005
audio_jitter_ms = 15
```

- Sender capture: Windows WASAPI loopback via `wasapi2src`.
- Encoding: `opusenc audio-type=restricted-lowdelay` with configurable frame size.
- Windows receiver playback: `opusdec` into `wasapi2sink low-latency=true`.
- Android receiver playback: RTP/Opus packets on `:5005` are decoded with `MediaCodec` and played through `AudioTrack`.
- Android sender capture: Android 10+ `AudioPlaybackCapture` records eligible app/game audio, encodes Opus with `MediaCodec`, and sends RTP/Opus to the receiver audio port.
- Audio stays CPU-side; libopus uses its own CPU/SIMD optimizations where available.
- Keep video on `5004` and audio on `5005` through the firewall.

## Icons

- The app uses a monochrome mirror-split display icon.
- Windows tray icon chooses black or white at startup based on the Windows system theme.
- The MSI includes both `screen-mirror.ico` and `screen-mirror-dark.ico`, both with alpha transparency.
- Android uses an adaptive launcher icon with a monochrome layer for themed icons.

## Android APK

Build the APK:

```powershell
.\scripts\build-apk.ps1 -Configuration Debug
```

The Android MVP includes:

- LAN receiver discovery compatible with the desktop app
- Android receiver mode using `MediaCodec` AVC decode to a `SurfaceView`
- Android sender mode using `MediaProjection` + `MediaCodec` AVC encode
- Sender fan-out to up to three discovered receivers
- Four-digit PIN pairing compatible with the desktop app
- Touch on the Android receiver surface sends normalized control events back to the sender
- Android sender touch injection through the bundled AccessibilityService after enabling `Screen Mirror` in Android Accessibility settings

## Touch Control

Touch is carried out-of-band from video:

- Android receiver touch events are sent to the active sender on UDP `47778`
- Windows desktop sender listens on UDP `47778`
- Android sender listens on UDP `47778` while sender mode is active and injects received events through Accessibility gestures
- Touch events include the same PIN hash and are ignored if it does not match
- Windows injects touch-equivalent mouse input with normalized coordinates

Current practical path:

```text
Android receiver touch -> Windows sender input, or Android sender input when the Screen Mirror AccessibilityService is enabled
```

Windows receiver-side touch capture needs a custom render window instead of the current GStreamer-created sink window. The protocol and sender-side injection are already separated so that can be added cleanly.

Android build requirements:

- Android SDK
- Gradle or a Gradle wrapper under `apps/android`

## Low-Latency Tuning

- Use wired LAN or 5GHz/6GHz Wi-Fi
- Keep receiver jitter low: `--jitter-ms 10` to `20`; default is `15`
- Keep `mtu = 1200` on Wi-Fi; try `mtu = 1400` only on a clean wired LAN
- Keep `udp_buffer_size = 4194304` for burst tolerance without building application-level latency
- Set `qos_dscp = 46` only if your router/switch honors DSCP; otherwise leave `-1`
- Use GPU encode/decode where available: `nvd3d11h264enc`, `mfh264enc`, `qsvh264enc`, `d3d11h264dec`
- Increase bitrate for desktop readability: `--bitrate 16000`
- For lowest audio latency, keep `audio_frame_ms = "5"` or `"2.5"` and `audio_jitter_ms = 10` to `20`

### Transfer pipeline

The desktop sender uses a low-latency RTP/UDP pipeline:

- D3D11 screen capture stays in GPU memory when the selected encoder accepts D3D11 input.
- Sender queues are leaky and capped at one frame so old frames are dropped instead of delayed.
- RTP/H.264 uses `aggregate-mode=zero-latency` and a configurable MTU.
- UDP send/receive buffers default to `4 MiB`.
- Receiver `udpsrc` disables sender-address metadata collection to avoid unnecessary per-packet work.

## Third-Party Licensing

The MSI bundles selected GStreamer runtime files, libopus, and VDD files. Third-party notices are installed under `licenses/`.

- GStreamer is LGPL-based and dynamically bundled.
- Opus/libopus is BSD-style licensed with royalty-free patent grants.
- GPL-only GStreamer plugins such as `gstx264.dll` and `gstlibav.dll` are intentionally not bundled.
- Receiver jitterbuffer starts after two packets and drops late packets instead of increasing latency.

The Android packet path is optimized for the hot loop:

- RTP sequence numbers use a plain integer because packetization runs on one encoder thread.
- Receiver target comparison avoids per-frame string allocation.
- Packet sending reuses `DatagramPacket` instances.
- Direct `ByteBuffer` copy uses one reusable view per encoded frame instead of one duplicate per RTP packet.
- The decoder is drained only when a full NAL unit has been queued.

## Limits

- DRM/protected content is out of scope.
- UDP favors latency over guaranteed delivery; unstable Wi-Fi can produce visible corruption.
- Android sender chooses an encoder-aligned resolution from discovered receiver display metadata, capped at `1920x1080` for latency.
- Android sender touch injection requires manually enabling the bundled `Screen Mirror` AccessibilityService in Android system settings.
- Desktop audio transfer is implemented for Windows sender/receiver. Android receiver audio playback and Android sender app-audio capture are implemented; Android sender audio is limited by Android's AudioPlaybackCapture rules and app opt-out behavior.
