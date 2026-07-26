param(
    [ValidateSet("Install", "List", "Enable", "Disable", "Remove", "Scan", "Extend")]
    [string] $Action = "Install"
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$driver = Join-Path $root "vdd\MttVDD.inf"
$devcon = Join-Path $root "vdd\devcon.exe"
$hardwareId = "Root\MttVDD"
$instancePattern = "ROOT\MTTVDD\*"
$statusFile = Join-Path $env:TEMP "ScreenMirror-vdd-status.txt"

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Start-ElevatedSelf {
    $arguments = @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        "`"$PSCommandPath`"",
        "-Action",
        $Action
    )
    Start-Process -FilePath "powershell.exe" -ArgumentList $arguments -Verb RunAs | Out-Null
}

function Assert-BundledFiles {
    if (-not (Test-Path -LiteralPath $driver)) {
        throw "Bundled Virtual Display Driver INF was not found: $driver"
    }

    if (-not (Test-Path -LiteralPath $devcon)) {
        throw "Bundled devcon.exe was not found: $devcon"
    }
}

function Get-BundledVddDevices {
    if (-not (Get-Command Get-PnpDevice -ErrorAction SilentlyContinue)) {
        return @()
    }

    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
        $_.InstanceId -like $instancePattern
    })
}

function Get-VirtualDisplayCandidates {
    if (-not (Get-Command Get-PnpDevice -ErrorAction SilentlyContinue)) {
        return @()
    }

    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
        $_.InstanceId -notlike $instancePattern -and (
            $_.FriendlyName -match "MttVDD|Virtual Display|VirtualDisplay|VDD" -or
            $_.Name -match "MttVDD|Virtual Display|VirtualDisplay|VDD"
        )
    })
}

function Invoke-Devcon {
    param([string[]] $Arguments)

    Assert-BundledFiles
    $process = Start-Process -FilePath $devcon -ArgumentList $Arguments -WindowStyle Hidden -PassThru -Wait
    if ($process.ExitCode -ne 0) {
        throw "devcon $($Arguments -join ' ') failed with exit code $($process.ExitCode)."
    }
}

function Invoke-DeviceScan {
    Start-Process -FilePath (Join-Path $env:WINDIR "System32\pnputil.exe") -ArgumentList "/scan-devices" -WindowStyle Hidden -Wait | Out-Null
}

function Invoke-ExtendDisplay {
    Start-Process -FilePath (Join-Path $env:WINDIR "System32\DisplaySwitch.exe") -ArgumentList "/extend" -WindowStyle Hidden | Out-Null
}

function Write-DeviceStatus {
    $devices = Get-BundledVddDevices
    $candidates = Get-VirtualDisplayCandidates
    $screenMirror = Join-Path $root "screen-mirror.exe"
    if (-not (Test-Path -LiteralPath $screenMirror)) {
        $repoExe = Join-Path (Split-Path -Parent $root) "target\release\screen-mirror.exe"
        if (Test-Path -LiteralPath $repoExe) {
            $screenMirror = $repoExe
        }
    }
    $monitors = if (Test-Path -LiteralPath $screenMirror) {
        & $screenMirror monitors 2>&1
    } else {
        "screen-mirror.exe was not found next to this script."
    }
    $content = @(
        "Screen Mirror Virtual Display Status",
        "Generated: $(Get-Date -Format s)",
        "",
        "Bundled VDD devices:"
    )

    if ($devices.Count -eq 0) {
        $content += "  none"
    } else {
        $content += ($devices |
            Select-Object Status, Class, FriendlyName, InstanceId |
            Format-Table -AutoSize |
            Out-String).TrimEnd()
    }

    $content += @(
        "",
        "Other virtual display candidates:"
    )
    if ($candidates.Count -eq 0) {
        $content += "  none"
    } else {
        $content += ($candidates |
            Select-Object Status, Class, FriendlyName, InstanceId |
            Format-Table -AutoSize |
            Out-String).TrimEnd()
    }

    $content += @(
        "",
        "Windows displays:",
        ($monitors | Out-String).TrimEnd()
    )
    $content | Set-Content -LiteralPath $statusFile -Encoding UTF8
    Start-Process -FilePath "notepad.exe" -ArgumentList "`"$statusFile`"" | Out-Null
}

if ($Action -in @("Install", "Enable", "Disable", "Remove", "Scan") -and -not (Test-Admin)) {
    Start-ElevatedSelf
    return
}

switch ($Action) {
    "Install" {
        Assert-BundledFiles
        $existing = Get-BundledVddDevices
        if ($existing.Count -eq 0) {
            Invoke-Devcon @("install", "`"$driver`"", $hardwareId)
        }
        Invoke-DeviceScan
        Invoke-ExtendDisplay
    }

    "List" {
        Write-DeviceStatus
    }

    "Enable" {
        $devices = Get-BundledVddDevices
        if ($devices.Count -eq 0) {
            throw "No bundled Virtual Display Driver device was found."
        }
        foreach ($device in $devices) {
            Invoke-Devcon @("enable", "@$($device.InstanceId)")
        }
        Invoke-DeviceScan
        Invoke-ExtendDisplay
    }

    "Disable" {
        $devices = Get-BundledVddDevices
        if ($devices.Count -eq 0) {
            throw "No bundled Virtual Display Driver device was found."
        }
        foreach ($device in $devices) {
            Invoke-Devcon @("disable", "@$($device.InstanceId)")
        }
        Invoke-DeviceScan
    }

    "Remove" {
        $devices = Get-BundledVddDevices
        if ($devices.Count -eq 0) {
            return
        }
        Add-Type -AssemblyName System.Windows.Forms
        $choice = [System.Windows.Forms.MessageBox]::Show(
            "Remove all bundled Root\MttVDD virtual display devices?",
            "Screen Mirror",
            [System.Windows.Forms.MessageBoxButtons]::YesNo,
            [System.Windows.Forms.MessageBoxIcon]::Warning
        )
        if ($choice -ne [System.Windows.Forms.DialogResult]::Yes) {
            return
        }
        foreach ($device in $devices) {
            Invoke-Devcon @("remove", "@$($device.InstanceId)")
        }
        Invoke-DeviceScan
    }

    "Scan" {
        Invoke-DeviceScan
    }

    "Extend" {
        Invoke-ExtendDisplay
    }
}
