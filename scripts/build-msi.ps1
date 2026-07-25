param(
    [string] $Configuration = "Release",
    [string] $ProductVersion = ""
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found. Install Rust first."
}

if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    throw "dotnet was not found. Install .NET SDK 6 or newer for WiX SDK builds."
}

if (-not $ProductVersion) {
    $desktopCargo = Join-Path $repo "apps\desktop\Cargo.toml"
    $versionLine = Select-String -Path $desktopCargo -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $versionLine) {
        throw "Could not read package version from $desktopCargo"
    }
    $ProductVersion = $versionLine.Matches[0].Groups[1].Value
}

if ($ProductVersion -notmatch '^\d+\.\d+\.\d+$') {
    throw "MSI ProductVersion must be three numeric fields, for example 0.1.0. Got: $ProductVersion"
}

Write-Host "Building desktop release..."
cargo build -p screen-mirror --release

$exe = Join-Path $repo "target\release\screen-mirror.exe"
if (-not (Test-Path -LiteralPath $exe)) {
    throw "Expected executable not found: $exe"
}

Write-Host "Building MSI..."
$wixProject = Join-Path $repo "installer\ScreenMirror.wixproj"
dotnet build $wixProject `
    -c $Configuration `
    -p:Platform=x64 `
    -p:ProductVersion=$ProductVersion `
    -p:AppExe=$exe

$msi = Get-ChildItem -Path (Join-Path $repo "installer") -Recurse -Filter "*.msi" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1

if (-not $msi) {
    throw "MSI build completed but no .msi file was found under installer\"
}

Write-Host "MSI: $($msi.FullName)"
