# screen-mirror

Low-latency LAN screen mirroring for Windows and Android. This is not Miracast-compatible; it uses a small custom discovery protocol plus RTP/H.264 over UDP.

## Monorepo Layout

- `apps/desktop`: Rust Windows tray app and CLI
- `apps/android`: native Android app project that builds an APK
- `crates/sm-core`: shared Rust protocol definitions
- `installer`: WiX SDK project for the Windows MSI
- `scripts`: local build/check scripts

## Release Status

The current published build is available from the [latest GitHub Release](https://github.com/tsuyoshi-otake/screen-mirror/releases/latest).

- `ScreenMirror.msi` is the current Windows distributable installer and the asset used by the desktop auto-updater.
- `ScreenMirror-Android-debug.apk` is a debug-signed APK for development and device testing only. It is not a production-signed Android release.
- To install or update the debug APK from a PC with Android platform tools and USB debugging enabled:

```powershell
adb install -r .\ScreenMirror-Android-debug.apk
```

The desktop and Android package versions for this release are `0.1.58`.

## Transport Model

- Discovery: UDP broadcast on port `47777`, plus PIN-filtered local-subnet unicast probes on UDP `47776` when broadcasts are suppressed
- Touch/control: UDP JSON on port `47778`
- Diagnostics: TCP JSON request/report response on port `47779`
- Video: RTP/H.264 over UDP on port `5004`
- Audio: optional Opus/RTP over UDP on port `5005`
- Pairing: sender and receiver must use the same four-digit PIN; discovery/control packets carry only a SHA-256 PIN hash
- Sender fan-out: one sender can stream to multiple receivers with `multiudpsink`
- Default auto-connect limit: `3` receivers, so `1:3` is the standard target
- Reconnect: receiver advertisements repeat once per second; after a stream disconnects, the receiver closes the render window but keeps listening and advertising for reconnection

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
- `Sender GPU` / `Receiver GPU`: pins encoding (sender) and decoding/rendering (receiver) to one GPU. Only shown when the machine has more than one GPU.
- `Open Display Settings`: opens Windows display settings
- `Open Config`: opens `%APPDATA%\screen-mirror\config.toml`

## Quick Start

### Windows to Android as an extended display

1. Install `ScreenMirror.msi` from the [latest GitHub Release](https://github.com/tsuyoshi-otake/screen-mirror/releases/latest).
2. For Android testing, install `ScreenMirror-Android-debug.apk` from the same release. This APK is debug-signed and is not a production build.
3. Start `Screen Mirror` from the Start Menu or the system tray.
4. Open the tray menu and run `Install/Repair Virtual Display Driver`.
5. Allow the UAC prompt, then confirm Windows Display Settings shows an extra display.
6. Set the same four-digit PIN on Windows and Android. The default is `0000`.
7. Start the Android app and tap `Start Receiver`.
8. On Windows, choose `Start as Sender` from the tray menu.
9. If needed, run `screen-mirror.exe monitors` and set `[send].monitor_index` in `%APPDATA%\screen-mirror\config.toml`.

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
config_version = 4

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
gpu = "auto"
fps = 30
bitrate = 8000
mtu = 1200
udp_buffer_size = 1048576
qos_dscp = -1
allow_software_encoder = false
nvidia_tuning = "auto"
zero_copy = true

[recv]
gpu = "auto"
audio_enabled = false
audio_port = 5005
audio_jitter_ms = 10
jitter_ms = 15
udp_buffer_size = 1048576
mtu = 1200
jitter_faststart_packets = 2
jitter_max_dropout_ms = 200
jitter_max_misorder_ms = 50
fullscreen = true
sampling = "auto"
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

The MSI bundles the signed VDD Driver Only package under the install directory. Use the tray menu item `Install/Repair Virtual Display Driver` to run an idempotent install. The app drives SetupAPI directly - no PowerShell and no `devcon.exe` - so the install runs through UAC in a hidden child process and creates the root-enumerated MTT VDD display device only if one does not already exist. Depending on Windows/driver state, the instance can appear as `ROOT\DISPLAY\...` with an attached `DISPLAY\MTT1337\...` monitor. `pnputil /add-driver` alone is not enough because it only stages/updates matching devices. If driver installation is blocked by policy or times out, install/update VDD manually from <https://github.com/VirtualDrivers/Virtual-Display-Driver/releases> or run:

```powershell
winget install --id=VirtualDrivers.Virtual-Display-Driver -e
```

Runtime behavior:

1. Start the receiver on the tablet/second PC.
2. Receiver discovery advertises its display resolution.
3. Start the desktop sender.
4. With `host = "auto"`, the sender waits until at least one receiver with a matching PIN is discovered.
5. After a matching receiver is found, the sender ensures the bundled MTT VDD device exists and is enabled. It requests Windows extended-display mode only when that display is not already capture-ready.
6. The sender then grows the driver to exactly one virtual monitor per receiver (`<monitors><count>` in `C:\VirtualDisplayDriver\vdd_settings.xml`, applied by restarting the device). After a restart it waits for the exact endpoint set, extends any detached endpoints, and verifies that the capture targets are stable.
7. Each virtual display is matched to the resolution of the receiver that shows it. The requested refresh rate is preferred; when only that rate is unsupported, Windows may select another rate for the same resolution.
8. Only after every VDD target and receiver route has passed those checks does the sender start the video pipelines. A preparation failure starts no stream, never falls back to a physical monitor, and is retried on the next discovery pass.
9. When auto sender mode loses every matching receiver beyond the disconnect grace period, it requests bundled VDD removal once so the virtual display is not left in Windows while disconnected.
10. Repeated receiver discovery does not rerun the driver actions or `DisplaySwitch.exe`; those operations run only when the receiver endpoint or its announced display mode changes.

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

### Protected video and DRM

The current sender captures a Windows display through DXGI or Windows Graphics Capture after the virtual monitor has been created. Windows intentionally prevents the Desktop Duplication API from exposing protected video content, so DRM-protected playback can appear black even when the rest of the desktop is visible. Creating a generic VDD does not remove that restriction because screen-mirror still recaptures the completed display.

There is no supported capture flag that bypasses this protection. A SuperDisplay-class implementation would require a screen-mirror-owned [IddCx indirect display driver](https://learn.microsoft.com/windows-hardware/drivers/display/indirect-display-driver-model-overview) that receives and transports the display swapchain directly and correctly implements Output Protection Manager behavior, driver signing, installation, and hardware/content-protection requirements. The bundled MTT VDD is only used to create a monitor and does not expose its swapchain to this app. The current release therefore does not claim support for Netflix or other DRM-protected video. This is an architectural limitation, not an encoder or bitrate setting.

Android has the equivalent platform restriction: windows marked [`FLAG_SECURE`](https://developer.android.com/reference/android/view/WindowManager.LayoutParams#FLAG_SECURE) are excluded from screenshots, non-secure displays, and media projection. Screen Mirror does not attempt to bypass content-provider protection on either platform.

## GPU Encoding

The sender uses D3D11 screen capture and prefers hardware encoders:

1. `nvd3d11h264enc` for NVIDIA GeForce GTX/RTX
2. `qsvh264enc` for Intel Quick Sync
3. `amfh264enc` for AMD Radeon
4. `mfh264enc` for Windows Media Foundation hardware encoders

By default `allow_software_encoder = false`, so `auto` will not silently fall back to CPU `x264enc`. Set it to `true` only if software fallback is acceptable, and only on a machine with a full GStreamer install: `x264enc` is GPL-licensed and the MSI does not bundle it, so the setting has no effect on an installed build. Receiving is unaffected - the installer does bundle a software *decoder*.

### Choosing a GPU

On a machine with more than one GPU - for example a GeForce RTX 4060 Ti next to Radeon Graphics - the sender and the receiver each pick their own GPU. The sender setting pins the encoder, the receiver setting pins the H.264 decoder and the video sink.

List the selectable GPUs:

```powershell
screen-mirror.exe gpus
```

Set them from the tray menu (`Sender GPU` / `Receiver GPU`), from `config.toml`, or per CLI run:

```toml
[send]
gpu = "NVIDIA GeForce RTX 4060 Ti"

[recv]
gpu = "AMD Radeon Graphics"
```

```powershell
screen-mirror.exe send --gpu "NVIDIA GeForce RTX 4060 Ti" --host 192.168.1.20
screen-mirror.exe recv --gpu 0
```

The value is `auto` (default), an adapter description, a case-insensitive substring of it such as `4060`, or a DXGI adapter index. A description that no longer matches an installed adapter falls back to `auto` and is logged. Machines with a single GPU do not show the tray submenus and do not need the keys at all.

The chosen GPU is applied by selecting the per-device element GStreamer registers for that adapter (`nvd3d11h264device1enc`, `qsvh264device1enc`, `amfh264device1enc`, `d3d12h264device1dec`, `d3d11h264device1dec`, and the matching sink `adapter` property). If the requested vendor has no encoder on that GPU, the sender logs the mismatch and keeps the automatic choice. `mfh264enc` has no adapter property, so it is never GPU-pinned.

For receivers, `auto` follows the GPU that owns the primary attached display. On Intel GPUs, a matching per-device D3D12/DXVA H.264 decoder plus D3D12 sink is selected as one zero-copy route when the active driver exposes both capabilities; this covers current Core Ultra and Arc hardware without a generation-name allowlist. If that D3D12 route reports a runtime error, the receiver retries once on the same GPU's compatible D3D11/Quick Sync route. Older Intel GPUs automatically retain the matching D3D11 route. NVIDIA, Radeon, Intel, and other adapters otherwise prefer the matching per-device D3D11/DXVA H.264 decoder and same-adapter D3D11 sink. NVIDIA can fall back to NVDEC and Intel can fall back to Quick Sync when a matching D3D11 decoder is unavailable; Radeon uses the Windows D3D11/DXVA decode path because the GStreamer AMF plugin is encoder-only. An explicit `gpu`, `decoder`, or `sink` setting still overrides its corresponding automatic choice.

Every automatic route ends with a bundled software decoder (`openh264dec`, BSD-licensed). Hardware decoders advertise fixed caps - Intel HD Graphics 4000 tops out at 1920x1920 and no DXVA decoder accepts 4:4:4 - and a stream outside those caps fails to negotiate before a single frame reaches the decoder, which used to stop the receiver with `not-negotiated (-4)` and no picture. The receiver now walks its routes in order (D3D12, then D3D11/Quick Sync, then software) and keeps the first one that plays. Software decode is 4:2:0-only and costs CPU, so it is never chosen first. Set `decoder = "software"` to pin it, or `decoder = "avdec"` to require libav on a machine with a full GStreamer install (the MSI does not bundle `gstlibav.dll`).

Desktop duplication always captures on the GPU that owns the captured monitor. When the selected encoder GPU is not that GPU, the sender drops out of the zero-copy path for that session and hands the encoder system-memory frames, because D3D11 textures cannot cross adapters.

Balanced sender defaults are `30 fps`, `8000 kbit/s`, and a `1 MiB` UDP buffer. `zero_copy = true` keeps D3D11 capture textures in GPU memory through NVIDIA, Quick Sync, AMF, and D3D11-aware Media Foundation encoders, avoiding a full-frame GPU-to-RAM copy. The app checks the installed encoder caps and automatically falls back to system-memory frames when that runtime cannot accept D3D11 textures. Set `zero_copy = false` only to diagnose an older driver or cross-adapter negotiation failure. Use `60 fps` as an explicit quality/latency tradeoff; it roughly doubles capture and encode work.

When a pre-v2 config still contains the old unchanged defaults (`60 fps`, `12000 kbit/s`, `4 MiB` buffers), it is migrated once to the balanced values. Nondefault user-tuned values are preserved.

When a pre-v3 config still contains the former audio defaults (`5 ms` Opus frames and `5` or `15 ms` audio jitter), it is migrated once to the stable low-latency defaults (`5 ms` frames and `10 ms` jitter). A v3 config using the aggressive `2.5 ms`/`3 ms` defaults is also migrated to `5 ms`/`10 ms`. Other explicitly tuned values are preserved.

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

The MSI installs `screen-mirror.exe`, the tray/start-menu icon, autostart registration, a local-subnet Windows Firewall exception for all network profiles, and the required GStreamer runtime DLL/plugin files.
The tray app is launched after install/update and is registered under HKCU Run for the installing Windows user.

## Diagnostics

Use tray menu `Run Diagnostics` to collect a local debug report. The report is saved under `%TEMP%`, opened in Notepad, and copied to the clipboard so it can be pasted into an issue or chat.
Run it while sender mode is actively mirroring: the `Video Encode` result is sampled live for five seconds, while the AMF and D3D11 results describe the last sender route recorded in the log.

Use tray menu `Run Peer Diagnostics` on a receiver to collect diagnostics from a sender with the same PIN. The sender advertises a diagnostics endpoint while sender mode is active, and the receiver requests the report over TCP `47779`. The received report is saved under `%TEMP%`, opened in Notepad, and copied to the receiver clipboard.

The report includes:

- Raw `%APPDATA%\screen-mirror\config.toml`, including the four-digit PIN
- Recent Screen Mirror log lines
- Update attempt state, last update-runner failure, and relevant MSI log lines
- Installed version, autostart entry, and running process list
- Per-process details and an aggregate summary for Screen Mirror CPU, RAM, GPU memory, GPU-engine usage, and `Video Decode` / Radeon `Video Codec` peak/current activity when Windows exposes those counters; negotiated D3D12/D3D11 memory is reported separately when a driver returns zero
- GPU acceleration verdict for Radeon AMF availability/selection, D3D11 zero-copy, and sampled `Video Encode` / AMD `Video Codec` engine activity
- Latest receiver playback route: selected hardware profile, PCI device ID, adapter LUID, decoder, negotiated D3D12/D3D11 memory path, and video sink
- Bundled VDD device status and other virtual display candidates
- Communication health verdict, Windows network profiles, active UDP/TCP endpoints, and the installed Screen Mirror firewall rule
- Windows display list, GStreamer probe, receiver discovery, network adapters, and virtual display state

## Auto Update

The tray app checks GitHub Releases automatically:

- First check: 30 seconds after tray startup
- Regular interval: once per hour
- Manual check: tray menu `Check for Updates`
- Asset name: `ScreenMirror.msi`
- Version lookup runs inside the Screen Mirror process over HTTPS; periodic checks only notify through the tray status and never download or start an installer
- Selecting `Check for Updates` downloads the MSI in-process, then requests one Windows UAC approval for the per-machine update
- Install mode: elevated hidden `msiexec.exe /i <msi> /qn /norestart`; the update runner and MSI process-stop helper do not create a console window
- The update runner waits for the exact old process ID to exit before invoking MSI
- Failed update details and the MSI log are retained for diagnostics; no automatic install retry is scheduled

## Receiver Power Behavior

Receiver mode prevents the display and system from sleeping while video reception is active:

- Windows receiver calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)`.
- Android receiver sets `FLAG_KEEP_SCREEN_ON` and `SurfaceView.setKeepScreenOn(true)`.
- The sleep/display guard is released when receiver mode stops.
- After video packets stop arriving, the Windows receiver destroys the fullscreen render pipeline, remains in receiver mode, and immediately starts a fresh video listener so the sender can reconnect without manual intervention.

## Receiver Window

The Windows receiver uses GStreamer `d3d11videosink` for GPU rendering, but screen-mirror renames the generated render window while receiver mode is active:

- Taskbar/window title: `screen-mirror Receiver`
- Window icon: the same simple display icon used by the app and tray
- Fullscreen: enabled by default with `[recv].fullscreen = true`

### Scaling Filter

When the sender's resolution and the receiver's window do not match, the sink scales the frame, and the texture filter it scales with decides whether text stays legible. Choose it from the tray under `Scaling Filter`, or set `[recv].sampling`:

| `sampling` | `sampling-method` | Effect |
| --- | --- | --- |
| `auto` (default) | `linear-minification` | Averages when shrinking so thin glyph strokes survive, and does not smear them when stretching |
| `linear` | `bilinear` | Filters in both directions; the GStreamer sink default, and softer on an upscaled stream |
| `point` | `nearest-neighbour` | No filtering at all; sharpest at 1:1, aliased anywhere else |

The setting applies to `d3d11videosink` and `d3d12videosink`, which spell the property identically. It takes effect when the sink is built, so changing it from the tray restarts an active receiver.

## Audio

Audio transfer is optional and uses a separate low-latency Opus/RTP stream:

- On each desktop endpoint, choose `Enable System Audio Transfer` from the tray menu. Video and audio run in independent pipelines, so toggling audio starts or stops only the audio transport and keeps the active video session connected.
- Audio must be enabled on both the sender and receiver. The tray status shows `audio on` or `audio off` for the active mode.

```toml
[send]
audio_enabled = true
audio_port = 5005
audio_bitrate = 96000
audio_frame_ms = "5"

[recv]
audio_enabled = true
audio_port = 5005
audio_jitter_ms = 10
```

- Sender capture: Windows WASAPI loopback via `wasapi2src`.
- Encoding: `opusenc audio-type=restricted-lowdelay` with configurable frame size.
- Windows receiver playback: `opusdec` into `wasapi2sink low-latency=true`.
- Android receiver playback: RTP/Opus packets on `:5005` are decoded with `MediaCodec` and played through low-latency `AudioTrack`. Playback starts with a 10 ms application buffer and expands in 5 ms steps only after an underrun.
- Android sender capture: Android 10+ `AudioPlaybackCapture` records eligible app/game audio, encodes Opus with `MediaCodec`, and sends RTP/Opus to the receiver audio port.
- Android sender and receiver audio controls also start or stop only their audio transport while video remains active.
- Audio stays CPU-side; libopus uses its own CPU/SIMD optimizations where available.
- Keep video on `5004` and audio on `5005` through the firewall.

## Icons

- The app uses a monochrome mirror-split display icon.
- Windows tray icon chooses black or white at startup based on the Windows system theme.
- The MSI includes both `screen-mirror.ico` and `screen-mirror-dark.ico`, both with alpha transparency.
- Android uses an adaptive launcher icon with a monochrome layer for themed icons.

## Android Debug APK

Build the APK:

```powershell
.\scripts\build-apk.ps1 -Configuration Debug
```

The Android implementation includes:

- LAN receiver discovery compatible with the desktop app
- Android receiver mode using `MediaCodec` AVC decode to a `SurfaceView`
- Android sender mode using `MediaProjection` + `MediaCodec` AVC encode
- A MediaProjection foreground service with a persistent notification and Stop action
- Sender fan-out to up to three discovered receivers
- Four-digit PIN pairing compatible with the desktop app
- Touch on the Android receiver surface sends normalized control events back to the sender
- Android sender touch injection through the bundled AccessibilityService after enabling `Screen Mirror` in Android Accessibility settings
- A three-second video disconnect timeout that exits the stale receiver screen
- Restart-safe UDP receiver shutdown, bounded touch-event queuing, and copyable in-app diagnostics
- Local JVM tests for PIN handling, RTP headers, sender profile limits, and disconnect timing

The build script runs `testDebugUnitTest`, `lintDebug`, and the selected APK build. The equivalent direct verification is:

```powershell
cd apps\android
.\gradlew.bat testDebugUnitTest lintDebug assembleDebug
```

Release builds require all four signing values as Gradle properties or environment variables:

```text
SCREEN_MIRROR_KEYSTORE_FILE
SCREEN_MIRROR_KEYSTORE_PASSWORD
SCREEN_MIRROR_KEY_ALIAS
SCREEN_MIRROR_KEY_PASSWORD
```

`assembleRelease` fails instead of silently producing an unsigned APK when those values are missing. Do not commit the keystore or credentials.

The GitHub Release asset is the debug APK produced by this workflow. Production Android distribution still requires a signed release build and bidirectional Windows/Android testing on physical Android hardware; local JVM tests cannot validate device-specific `MediaCodec`, `MediaProjection`, audio-capture, or Accessibility behavior.

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
- Keep `udp_buffer_size = 1048576` for low memory usage with adequate burst tolerance; raise it only after observing packet drops
- Set `qos_dscp = 46` only if your router/switch honors DSCP; otherwise leave `-1`
- Use GPU encode/decode where available: `nvd3d11h264enc`, `mfh264enc`, `qsvh264enc`, `d3d12h264dec`, `d3d11h264dec`
- Increase bitrate for desktop readability: `--bitrate 16000`
- For stable low audio latency, keep `audio_frame_ms = "5"` and `audio_jitter_ms = 10`; raise jitter to `15` on unstable Wi-Fi

### Transfer pipeline

The desktop sender uses a low-latency RTP/UDP pipeline:

- D3D11 screen capture stays in GPU memory for NVIDIA, Quick Sync, AMF, and D3D11-aware Media Foundation encoders when `zero_copy = true`, and only while the encoder runs on the capture GPU.
- When an encoder requires system-memory input, color conversion and scaling still run on the capture GPU before downloading compact NV12 frames, avoiding CPU-side BGRA conversion and reducing the downloaded bytes per pixel.
- AMD AMF uses its ultra-low-latency usage and speed preset. The Media Foundation fallback also selects its fastest live-encoding quality/speed mode.
- Sender queues are leaky and capped at one frame so old frames are dropped instead of delayed.
- Audio uses 5 ms Opus frames, short bounded burst queues, two-packet jitter-buffer fast start, Opus packet-loss concealment, and the native WASAPI low-latency mode.
- RTP/H.264 uses `aggregate-mode=zero-latency` and a configurable MTU.
- The sender requests an immediate H.264 keyframe with headers and repeats that request every second, so a receiver can start or recover without waiting for an encoder-specific GOP implementation.
- UDP send/receive buffers default to `1 MiB`.
- Receiver `udpsrc` disables sender-address metadata collection to avoid unnecessary per-packet work.
- `Run Diagnostics` records process CPU, working set/private memory, handle/thread counts, GPU process memory, per-engine utilization samples, and `Video Decode` / Radeon `Video Codec` peak/current usage when available. GPU counter support varies by Windows driver; unavailable counters do not prevent the report from being generated.

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
- Android sender chooses an encoder-aligned resolution from discovered receiver display metadata, capped at `1920x1080@30` for balanced GPU load and latency.
- Android sender touch injection requires manually enabling the bundled `Screen Mirror` AccessibilityService in Android system settings.
- Desktop audio transfer is implemented for Windows sender/receiver. Android receiver audio playback and Android sender app-audio capture are implemented; Android sender audio is limited by Android's AudioPlaybackCapture rules and app opt-out behavior.
