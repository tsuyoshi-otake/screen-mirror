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
- Video: RTP/H.264 over UDP on port `5004`
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
- `Open Config`: opens `%APPDATA%\screen-mirror\config.toml`

## Quick Start

### Windows to Android as an extended display

1. Install `ScreenMirror.msi` from the latest GitHub Release.
2. Start `Screen Mirror` from the Start Menu or the system tray.
3. Open the tray menu and run `Install Bundled Virtual Display Driver`.
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
- Android stores the PIN from the app input field and reuses it on the next launch.
- Auto discovery ignores receivers with a different PIN hash, so mismatched devices do not auto-connect.
- This is pairing protection for trusted LAN use, not strong encryption. RTP/H.264 video is still sent over plain UDP.

## Virtual Display Mode

For a SuperDisplay-like extended desktop workflow, screen-mirror uses Virtual Display Driver (VDD) as the Windows virtual monitor and streams that display.

The MSI bundles the signed VDD Driver Only package and `devcon.exe` from the official VDD Control release under the install directory. Use the tray menu item `Install Bundled Virtual Display Driver` to run:

```powershell
devcon.exe install "vdd\MttVDD.inf" Root\MttVDD
```

This launches the driver install through UAC and creates the root-enumerated `Root\MttVDD` device. `pnputil /add-driver` alone is not enough because it only stages/updates matching devices. If driver installation is blocked by policy or times out, install/update VDD manually from <https://github.com/VirtualDrivers/Virtual-Display-Driver/releases> or run:

```powershell
winget install --id=VirtualDrivers.Virtual-Display-Driver -e
```

Runtime behavior:

1. Start the receiver on the tablet/second PC.
2. Receiver discovery advertises its display resolution.
3. Start the desktop sender.
4. The sender requests Windows extended-display mode with `DisplaySwitch.exe /extend`.
5. If a VDD/SuperDisplay-style virtual monitor is visible, the sender tries to match its resolution to the first receiver.
6. With `prefer_virtual_display = true` and `monitor_index = -1`, the sender captures that virtual monitor and falls back to the primary monitor if none is found.

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

## Auto Update

The tray app checks GitHub Releases automatically:

- First check: 30 seconds after tray startup
- Regular interval: once per hour
- Manual check: tray menu `Check for Updates`
- Asset name: `ScreenMirror.msi`
- Install mode: quiet MSI update with `/qn /norestart`

## Receiver Power Behavior

Receiver mode prevents the display and system from sleeping while video reception is active:

- Windows receiver calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)`.
- Android receiver sets `FLAG_KEEP_SCREEN_ON` and `SurfaceView.setKeepScreenOn(true)`.
- The sleep/display guard is released when receiver mode stops.

## Receiver Window

The Windows receiver uses GStreamer `d3d11videosink` for GPU rendering, but screen-mirror renames the generated render window while receiver mode is active:

- Taskbar/window title: `screen-mirror Receiver`
- Window icon: the same simple display icon used by the app and tray
- Fullscreen: enabled by default with `[recv].fullscreen = true`

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

## Touch Control

Touch is carried out-of-band from video:

- Android receiver touch events are sent to the active sender on UDP `47778`
- Windows desktop sender listens on UDP `47778`
- Touch events include the same PIN hash and are ignored if it does not match
- Windows injects touch-equivalent mouse input with normalized coordinates

Current practical path:

```text
Android receiver touch -> Windows sender input
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

### Transfer pipeline

The desktop sender uses a low-latency RTP/UDP pipeline:

- D3D11 screen capture stays in GPU memory when the selected encoder accepts D3D11 input.
- Sender queues are leaky and capped at one frame so old frames are dropped instead of delayed.
- RTP/H.264 uses `aggregate-mode=zero-latency` and a configurable MTU.
- UDP send/receive buffers default to `4 MiB`.
- Receiver `udpsrc` disables sender-address metadata collection to avoid unnecessary per-packet work.
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
- Android sender currently uses a fixed `1280x720@30` encode path.
- Android touch injection into Android sender devices is not implemented because it requires an AccessibilityService/root-level privileges.
- Audio is not implemented yet; the next step is WASAPI loopback on Windows and AudioPlaybackCapture on Android.
