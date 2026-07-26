$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$driver = Join-Path $root "vdd\MttVDD.inf"
$devcon = Join-Path $root "vdd\devcon.exe"

if (-not (Test-Path -LiteralPath $driver)) {
    throw "Bundled Virtual Display Driver INF was not found: $driver"
}

if (-not (Test-Path -LiteralPath $devcon)) {
    throw "Bundled devcon.exe was not found: $devcon"
}

$arguments = @("install", "`"$driver`"", "Root\MttVDD")
$process = Start-Process -FilePath $devcon -ArgumentList $arguments -Verb RunAs -PassThru

if (-not $process.WaitForExit(60000)) {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    throw "Virtual Display Driver installation timed out."
}

if ($process.ExitCode -ne 0) {
    throw "Virtual Display Driver installation failed with exit code $($process.ExitCode)."
}

Start-Process -FilePath (Join-Path $env:WINDIR "System32\pnputil.exe") -ArgumentList "/scan-devices" -WindowStyle Hidden -Wait | Out-Null
Start-Process -FilePath (Join-Path $env:WINDIR "System32\DisplaySwitch.exe") -ArgumentList "/extend" -WindowStyle Hidden | Out-Null
