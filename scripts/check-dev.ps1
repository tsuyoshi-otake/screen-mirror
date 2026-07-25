# Installs are intentionally not automated because GStreamer Windows package IDs differ by environment.
# Use this script as a local sanity check after installing Rust and GStreamer MSVC x86_64.

$ErrorActionPreference = "Stop"

Write-Host "Checking Rust..."
rustc --version
cargo --version

Write-Host "Checking GStreamer..."
if (-not $env:GSTREAMER_1_0_ROOT_MSVC_X86_64) {
    Write-Warning "GSTREAMER_1_0_ROOT_MSVC_X86_64 is not set. Example: C:\gstreamer\1.0\msvc_x86_64"
}

gst-inspect-1.0 --version
gst-inspect-1.0 d3d11screencapturesrc | Out-Host
gst-inspect-1.0 mfh264enc | Out-Host

Write-Host "Building..."
cargo build -p screen-mirror
