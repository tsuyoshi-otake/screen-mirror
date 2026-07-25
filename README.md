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
- `Open Config`: opens `%APPDATA%\screen-mirror\config.toml`

Default sender config uses:

```toml
[send]
host = "auto"
port = 5004
max_receivers = 3
fps = 60
bitrate = 12000
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
- Touch on the Android receiver surface sends normalized control events back to the sender

## Touch Control

Touch is carried out-of-band from video:

- Android receiver touch events are sent to the active sender on UDP `47778`
- Windows desktop sender listens on UDP `47778`
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
- Keep receiver jitter low: `--jitter-ms 10` to `20`
- Use GPU encode/decode where available: `nvd3d11h264enc`, `mfh264enc`, `qsvh264enc`, `d3d11h264dec`
- Increase bitrate for desktop readability: `--bitrate 16000`

## Limits

- DRM/protected content is out of scope.
- UDP favors latency over guaranteed delivery; unstable Wi-Fi can produce visible corruption.
- Android sender currently uses a fixed `1280x720@30` encode path.
- Android touch injection into Android sender devices is not implemented because it requires an AccessibilityService/root-level privileges.
- Audio is not implemented yet; the next step is WASAPI loopback on Windows and AudioPlaybackCapture on Android.
