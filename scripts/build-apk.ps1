param(
    [ValidateSet("Debug", "Release")]
    [string] $Configuration = "Debug"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$androidDir = Join-Path $repo "apps\android"
$gradlew = Join-Path $androidDir "gradlew.bat"

if (Test-Path -LiteralPath $gradlew) {
    $gradle = $gradlew
} elseif (Get-Command gradle -ErrorAction SilentlyContinue) {
    $gradle = "gradle"
} else {
    throw "Gradle was not found. Install Gradle or add a Gradle wrapper under apps\android."
}

Push-Location $androidDir
try {
    & $gradle "assemble$Configuration"
} finally {
    Pop-Location
}

$apk = Get-ChildItem -Path (Join-Path $androidDir "app\build\outputs\apk") -Recurse -Filter "*.apk" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1

if (-not $apk) {
    throw "APK build completed but no .apk file was found."
}

Write-Host "APK: $($apk.FullName)"
