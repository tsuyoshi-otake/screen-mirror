param(
    [ValidateSet("Install", "List", "Enable", "Disable", "Remove", "Scan", "Extend")]
    [string] $Action = "Install",

    [switch] $Force
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$driver = Join-Path $root "vdd\MttVDD.inf"
$devcon = Join-Path $root "vdd\devcon.exe"
$hardwareId = "Root\MttVDD"
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
        "-WindowStyle",
        "Hidden",
        "-File",
        "`"$PSCommandPath`"",
        "-Action",
        $Action
    )
    if ($Force) {
        $arguments += "-Force"
    }
    Start-Process -FilePath "powershell.exe" -ArgumentList $arguments -Verb RunAs -WindowStyle Hidden | Out-Null
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
        ($_.InstanceId -like "ROOT\MTTVDD\*") -or
        ($_.Class -eq "Display" -and $_.InstanceId -like "ROOT\DISPLAY\*" -and (
            $_.FriendlyName -match "Virtual Display Driver|MttVDD|VDD" -or
            $_.Name -match "Virtual Display Driver|MttVDD|VDD"
        ))
    })
}

function Get-BundledVddMonitors {
    if (-not (Get-Command Get-PnpDevice -ErrorAction SilentlyContinue)) {
        return @()
    }

    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
        ($_.Class -eq "Monitor" -and $_.InstanceId -like "DISPLAY\MTT1337\*") -or
        ($_.Class -eq "Monitor" -and (
            $_.FriendlyName -match "VDD by MTT|MttVDD" -or
            $_.Name -match "VDD by MTT|MttVDD"
        ))
    })
}

function Get-VirtualDisplayCandidates {
    if (-not (Get-Command Get-PnpDevice -ErrorAction SilentlyContinue)) {
        return @()
    }

    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
        $_.Class -in @("Display", "Monitor") -and
        $_.InstanceId -notlike "ROOT\MTTVDD\*" -and
        $_.InstanceId -notlike "ROOT\DISPLAY\*" -and
        $_.InstanceId -notlike "DISPLAY\MTT1337\*" -and (
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

function Invoke-PnPUtil {
    param([string[]] $Arguments)

    $pnputil = Join-Path $env:WINDIR "System32\pnputil.exe"
    $process = Start-Process -FilePath $pnputil -ArgumentList $Arguments -WindowStyle Hidden -PassThru -Wait
    if ($process.ExitCode -ne 0) {
        throw "pnputil $($Arguments -join ' ') failed with exit code $($process.ExitCode)."
    }
}

function Invoke-DeviceAction {
    param(
        [ValidateSet("enable", "disable", "remove")]
        [string] $Action,
        [string] $InstanceId
    )

    switch ($Action) {
        "enable" {
            try {
                Invoke-PnPUtil @("/enable-device", $InstanceId)
            } catch {
                Invoke-Devcon @("enable", "@$InstanceId")
            }
        }
        "disable" {
            try {
                Invoke-PnPUtil @("/disable-device", $InstanceId)
            } catch {
                Invoke-Devcon @("disable", "@$InstanceId")
            }
        }
        "remove" {
            try {
                Invoke-PnPUtil @("/remove-device", $InstanceId)
            } catch {
                Invoke-Devcon @("remove", "@$InstanceId")
            }
        }
    }
}

function Invoke-DeviceScan {
    Start-Process -FilePath (Join-Path $env:WINDIR "System32\pnputil.exe") -ArgumentList "/scan-devices" -WindowStyle Hidden -Wait | Out-Null
}

function Invoke-ScreenMirror {
    param(
        [string] $ExePath,
        [string[]] $Arguments
    )

    if (-not (Test-Path -LiteralPath $ExePath)) {
        return "screen-mirror.exe was not found."
    }

    $suffix = [Guid]::NewGuid().ToString("N")
    $stdoutPath = Join-Path $env:TEMP ("ScreenMirror-vdd-stdout-{0}.txt" -f $suffix)
    $stderrPath = Join-Path $env:TEMP ("ScreenMirror-vdd-stderr-{0}.txt" -f $suffix)
    try {
        $process = Start-Process -FilePath $ExePath `
            -ArgumentList $Arguments `
            -WindowStyle Hidden `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -Wait `
            -PassThru

        $output = ""
        if (Test-Path -LiteralPath $stdoutPath) {
            $output += Get-Content -LiteralPath $stdoutPath -Raw
        }
        if (Test-Path -LiteralPath $stderrPath) {
            $errorText = Get-Content -LiteralPath $stderrPath -Raw
            if (-not [string]::IsNullOrWhiteSpace($errorText)) {
                $output += $errorText
            }
        }
        if ([string]::IsNullOrWhiteSpace($output)) {
            return ("(no output; exit code {0})" -f $process.ExitCode)
        }
        return $output.TrimEnd()
    } finally {
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-ExtendDisplay {
    Start-Process -FilePath (Join-Path $env:WINDIR "System32\DisplaySwitch.exe") -ArgumentList "/extend" -WindowStyle Hidden | Out-Null
}

function Write-DeviceStatus {
    $devices = Get-BundledVddDevices
    $monitors = Get-BundledVddMonitors
    $candidates = Get-VirtualDisplayCandidates
    $screenMirror = Join-Path $root "screen-mirror.exe"
    if (-not (Test-Path -LiteralPath $screenMirror)) {
        $installedExe = Join-Path $env:ProgramFiles "Screen Mirror\screen-mirror.exe"
        if (Test-Path -LiteralPath $installedExe) {
            $screenMirror = $installedExe
        } else {
            $repoExe = Join-Path (Split-Path -Parent $root) "target\release\screen-mirror.exe"
            if (Test-Path -LiteralPath $repoExe) {
                $screenMirror = $repoExe
            }
        }
    }
    $windowsDisplays = Invoke-ScreenMirror $screenMirror @("monitors")
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
        "Bundled VDD monitors:"
    )
    if ($monitors.Count -eq 0) {
        $content += "  none"
    } else {
        $content += ($monitors |
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
        ($windowsDisplays | Out-String).TrimEnd()
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
    }

    "List" {
        Write-DeviceStatus
    }

    "Enable" {
        $devices = Get-BundledVddDevices
        $monitors = Get-BundledVddMonitors
        if ($devices.Count -eq 0 -and $monitors.Count -eq 0) {
            throw "No bundled Virtual Display Driver device was found."
        }
        foreach ($device in $devices) {
            Invoke-DeviceAction "enable" $device.InstanceId
        }
        foreach ($monitor in $monitors) {
            Invoke-DeviceAction "enable" $monitor.InstanceId
        }
        Invoke-DeviceScan
    }

    "Disable" {
        $devices = Get-BundledVddDevices
        $monitors = Get-BundledVddMonitors
        if ($devices.Count -eq 0 -and $monitors.Count -eq 0) {
            throw "No bundled Virtual Display Driver device was found."
        }
        foreach ($monitor in $monitors) {
            Invoke-DeviceAction "disable" $monitor.InstanceId
        }
        foreach ($device in $devices) {
            Invoke-DeviceAction "disable" $device.InstanceId
        }
        Invoke-DeviceScan
    }

    "Remove" {
        $devices = Get-BundledVddDevices
        $monitors = Get-BundledVddMonitors
        if ($devices.Count -eq 0 -and $monitors.Count -eq 0) {
            return
        }
        if (-not $Force) {
            Add-Type -AssemblyName System.Windows.Forms
            $choice = [System.Windows.Forms.MessageBox]::Show(
                "Remove all bundled MTT virtual display devices and monitors?",
                "Screen Mirror",
                [System.Windows.Forms.MessageBoxButtons]::YesNo,
                [System.Windows.Forms.MessageBoxIcon]::Warning
            )
            if ($choice -ne [System.Windows.Forms.DialogResult]::Yes) {
                return
            }
        }
        foreach ($monitor in $monitors) {
            Invoke-DeviceAction "remove" $monitor.InstanceId
        }
        foreach ($device in $devices) {
            Invoke-DeviceAction "remove" $device.InstanceId
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
