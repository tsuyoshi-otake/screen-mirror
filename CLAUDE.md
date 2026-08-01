# screen-mirror project notes

## Project shape

- `apps/desktop` is the Windows Rust tray app and CLI.
- `apps/android` is the Android sender/receiver application.
- `crates/sm-core` contains the shared discovery and control protocol.
- Video is H.264 RTP on UDP `5004`; audio is optional Opus RTP on UDP `5005`.

## Verification

Run these from the repository root before handing off changes:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

The Windows runtime also needs GStreamer. `cargo run -p screen-mirror -- probe` checks the bundled
element availability.

## Current video controls

- The tray exposes encoder, quality preset, bitrate, output resolution, frame rate, NVIDIA tuning,
  and receiver scaling-filter menus.
- `send.fec_percentage` defaults to `0`. Nonzero values use RFC 5109 ULP-FEC with a separate RTP
  payload. Automatic discovery enables it only when every selected desktop receiver advertises
  support; Android and older peers are treated as unsupported. An explicit host list is an
  operator assertion that the peers support FEC.
- Do not change the Android RTP parser casually: it intentionally accepts the existing H.264
  payload path and does not recover FEC packets.

## Release checklist

1. Bump `apps/desktop/Cargo.toml`, `Cargo.lock`, `apps/android/app/build.gradle`, and the release
   version text in `README.md` together.
2. Run the Rust verification commands above.
3. Build the Windows asset with
   `.\scripts\build-msi.ps1 -Configuration Release`.
4. Build the GitHub Android asset with
   `.\scripts\build-apk.ps1 -Configuration Debug`.
5. Commit the release changes, create an annotated `vX.Y.Z` tag, push `main` and the tag, then
   publish `ScreenMirror.msi` and `ScreenMirror-Android-debug.apk` to the matching GitHub release.

The Android release asset is intentionally debug-signed for development/device testing. Never
commit keystores, signing credentials, or generated release artifacts.
