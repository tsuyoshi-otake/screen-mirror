param(
    [string] $Svg = "assets\screen-mirror.svg",
    [string] $Ico = "assets\screen-mirror.ico"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$svgPath = Join-Path $repo $Svg
$icoPath = Join-Path $repo $Ico

if (-not (Test-Path -LiteralPath $svgPath)) {
    throw "SVG icon source not found: $svgPath"
}

$magick = Get-Command magick -ErrorAction SilentlyContinue
if (-not $magick) {
    $magickExe = Get-ChildItem -Path $env:ProgramFiles -Directory -Filter "ImageMagick*" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -Filter magick.exe -ErrorAction SilentlyContinue } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($magickExe) {
        $magick = $magickExe.FullName
    }
}

if (-not $magick) {
    throw "ImageMagick 'magick' was not found. Install ImageMagick to generate the Windows ICO."
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $icoPath) | Out-Null
& $magick $svgPath -background none -define icon:auto-resize=256,128,64,48,32,16 $icoPath

if (-not (Test-Path -LiteralPath $icoPath)) {
    throw "ICO generation failed: $icoPath"
}
