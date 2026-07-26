param(
    [string] $Svg = "assets\screen-mirror.svg",
    [string] $Ico = "assets\screen-mirror.ico",
    [string] $DarkSvg = "assets\screen-mirror-dark.svg",
    [string] $DarkIco = "assets\screen-mirror-dark.ico"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$svgPath = Join-Path $repo $Svg
$icoPath = Join-Path $repo $Ico
$darkSvgPath = Join-Path $repo $DarkSvg
$darkIcoPath = Join-Path $repo $DarkIco

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

function New-TransparentIco([string] $sourceSvg, [string] $targetIco) {
    if (-not (Test-Path -LiteralPath $sourceSvg)) {
        throw "SVG icon source not found: $sourceSvg"
    }
    $temp = Join-Path $env:TEMP ("screen-mirror-icon-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $temp | Out-Null
    try {
        $pngs = @()
        foreach ($size in @(256, 128, 64, 48, 32, 16)) {
            $png = Join-Path $temp "icon-$size.png"
            & $magick -background none $sourceSvg -resize "${size}x${size}" "PNG32:$png"
            $pngs += $png
        }
        & $magick @pngs $targetIco
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    }

    if (-not (Test-Path -LiteralPath $targetIco)) {
        throw "ICO generation failed: $targetIco"
    }
}

New-TransparentIco $svgPath $icoPath
New-TransparentIco $darkSvgPath $darkIcoPath
