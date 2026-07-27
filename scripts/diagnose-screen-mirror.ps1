param(
    [int] $DiscoveryTimeoutMs = 3000,

    [switch] $NoClipboard,

    [switch] $NoNotepad,

    [switch] $Stdout
)

$ErrorActionPreference = "Continue"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
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

$configPath = Join-Path $env:APPDATA "screen-mirror\config.toml"
$logPath = Join-Path $env:LOCALAPPDATA "ScreenMirror\screen-mirror.log"
$updatesPath = Join-Path $env:LOCALAPPDATA "ScreenMirror\Updates"
$reportPath = Join-Path $env:TEMP ("ScreenMirror-diagnostics-{0}.txt" -f (Get-Date -Format "yyyyMMdd-HHmmss"))
$interestingPorts = @(47776, 47777, 47778, 47779, 5004, 5005)
$lines = New-Object System.Collections.Generic.List[string]

function Add-Line([string] $Line = "") {
    $script:lines.Add($Line) | Out-Null
}

function Add-Section([string] $Title) {
    Add-Line ""
    Add-Line ("==== {0} ====" -f $Title)
}

function Add-CommandOutput([string] $Title, [scriptblock] $Command) {
    Add-Section $Title
    try {
        $output = & $Command 2>&1 | Out-String
        if ([string]::IsNullOrWhiteSpace($output)) {
            Add-Line "(no output)"
        } else {
            Add-Line ($output.TrimEnd())
        }
    } catch {
        Add-Line ("ERROR: {0}" -f $_.Exception.Message)
    }
}

function Invoke-ScreenMirror([string[]] $Arguments) {
    if (-not (Test-Path -LiteralPath $screenMirror)) {
        return "screen-mirror.exe not found"
    }

    $suffix = [Guid]::NewGuid().ToString("N")
    $stdoutPath = Join-Path $env:TEMP ("ScreenMirror-diag-stdout-{0}.txt" -f $suffix)
    $stderrPath = Join-Path $env:TEMP ("ScreenMirror-diag-stderr-{0}.txt" -f $suffix)
    try {
        $process = Start-Process -FilePath $screenMirror `
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

function Read-Pin {
    if (-not (Test-Path -LiteralPath $configPath)) {
        return "0000"
    }
    $match = Select-String -LiteralPath $configPath -Pattern '^\s*pin\s*=\s*"([^"]+)"' | Select-Object -First 1
    if ($match) {
        return $match.Matches[0].Groups[1].Value
    }
    "0000"
}

function Get-MonitorHandles {
    $source = @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class ScreenMirrorNativeMonitors {
    public struct Entry {
        public string Device;
        public IntPtr Handle;
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct MONITORINFOEX {
        public int cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public int dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string szDevice;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    private delegate bool MonitorEnumProc(IntPtr hMonitor, IntPtr hdcMonitor, ref RECT lprcMonitor, IntPtr dwData);

    [DllImport("user32.dll")]
    private static extern bool EnumDisplayMonitors(IntPtr hdc, IntPtr lprcClip, MonitorEnumProc lpfnEnum, IntPtr dwData);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFOEX lpmi);

    public static Entry[] Enumerate() {
        var entries = new List<Entry>();
        EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, delegate(IntPtr handle, IntPtr dc, ref RECT rect, IntPtr data) {
            var info = new MONITORINFOEX();
            info.cbSize = Marshal.SizeOf(typeof(MONITORINFOEX));
            if (GetMonitorInfo(handle, ref info)) {
                entries.Add(new Entry {
                    Device = info.szDevice,
                    Handle = handle,
                    Left = info.rcMonitor.Left,
                    Top = info.rcMonitor.Top,
                    Right = info.rcMonitor.Right,
                    Bottom = info.rcMonitor.Bottom
                });
            }
            return true;
        }, IntPtr.Zero);
        return entries.ToArray();
    }
}
"@
    try {
        if (-not ([System.Management.Automation.PSTypeName]'ScreenMirrorNativeMonitors').Type) {
            Add-Type -TypeDefinition $source -ErrorAction Stop
        }
        [ScreenMirrorNativeMonitors]::Enumerate() | ForEach-Object {
            [PSCustomObject]@{
                Device = $_.Device
                HMonitor = $_.Handle.ToInt64()
                Bounds = ("{0},{1} {2}x{3}" -f $_.Left, $_.Top, ($_.Right - $_.Left), ($_.Bottom - $_.Top))
            }
        }
    } catch {
        "ERROR: $($_.Exception.Message)"
    }
}

function Invoke-GstInspect([string[]] $Arguments) {
    $candidates = @()
    $appDir = Split-Path -Parent $screenMirror
    $candidates += (Join-Path $appDir "bin\gst-inspect-1.0.exe")
    $candidates += (Join-Path $env:ProgramFiles "gstreamer\1.0\msvc_x86_64\bin\gst-inspect-1.0.exe")
    $candidates += "gst-inspect-1.0.exe"
    $exe = $candidates | Where-Object { Get-Command $_ -ErrorAction SilentlyContinue } | Select-Object -First 1
    if (-not $exe) {
        return "gst-inspect-1.0.exe not found"
    }
    & $exe @Arguments 2>&1 | Out-String
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

$pin = Read-Pin

Add-Line "Screen Mirror Diagnostics"
Add-Line ("Generated: {0}" -f (Get-Date -Format s))
Add-Line ("User: {0}\{1}" -f $env:USERDOMAIN, $env:USERNAME)
Add-Line ("Computer: {0}" -f $env:COMPUTERNAME)
Add-Line ("Script: {0}" -f $MyInvocation.MyCommand.Path)
Add-Line ("Executable: {0}" -f $screenMirror)
Add-Line ("Config: {0}" -f $configPath)
Add-Line ("Report: {0}" -f $reportPath)
Add-Line "Note: this report includes the raw Screen Mirror config, including the four-digit PIN."

Add-CommandOutput "Installed Package" {
    Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq 'Screen Mirror' } |
        Select-Object DisplayName, DisplayVersion, Publisher, InstallLocation, UninstallString |
        Format-List
}

Add-CommandOutput "Autostart" {
    Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name ScreenMirror -ErrorAction SilentlyContinue |
        Select-Object ScreenMirror |
        Format-List
}

Add-CommandOutput "Running Processes" {
    Get-Process screen-mirror -ErrorAction SilentlyContinue |
        Select-Object Id, ProcessName, Path, StartTime, CPU, WorkingSet64, PrivateMemorySize64, HandleCount |
        Format-Table -AutoSize
}

Add-CommandOutput "CPU, Memory, and GPU Usage" {
    $processes = @(Get-Process screen-mirror -ErrorAction SilentlyContinue)
    if ($processes.Count -eq 0) {
        "screen-mirror.exe is not running."
        return
    }

    $cpuBefore = @{}
    foreach ($process in $processes) {
        $cpuBefore[$process.Id] = $process.CPU
    }
    Start-Sleep -Seconds 1

    $resourceRows = foreach ($process in $processes) {
        $current = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
        if (-not $current) {
            continue
        }
        [pscustomobject]@{
            Id = $current.Id
            CpuPercent = [math]::Round((($current.CPU - $cpuBefore[$current.Id]) * 100 / [Environment]::ProcessorCount), 2)
            WorkingSetMiB = [math]::Round($current.WorkingSet64 / 1MB, 1)
            PrivateMiB = [math]::Round($current.PrivateMemorySize64 / 1MB, 1)
            VirtualMiB = [math]::Round($current.VirtualMemorySize64 / 1MB, 1)
            Handles = $current.HandleCount
            Threads = $current.Threads.Count
        }
    }
    $resourceRows | Format-Table -AutoSize

    if (-not (Get-Command Get-Counter -ErrorAction SilentlyContinue)) {
        "GPU performance counters are unavailable."
        return
    }

    foreach ($process in $processes) {
        try {
            $memory = Get-Counter -Counter `
                "\GPU Process Memory(pid_$($process.Id)*)\Dedicated Usage", `
                "\GPU Process Memory(pid_$($process.Id)*)\Shared Usage" `
                -ErrorAction Stop
            $dedicated = @($memory.CounterSamples | Where-Object Path -Like "*\Dedicated Usage" |
                Measure-Object CookedValue -Sum).Sum
            $shared = @($memory.CounterSamples | Where-Object Path -Like "*\Shared Usage" |
                Measure-Object CookedValue -Sum).Sum
            [pscustomobject]@{
                Id = $process.Id
                GpuDedicatedMiB = [math]::Round($dedicated / 1MB, 1)
                GpuSharedMiB = [math]::Round($shared / 1MB, 1)
            } | Format-Table -AutoSize

            $engines = Get-Counter -Counter `
                "\GPU Engine(pid_$($process.Id)*)\Utilization Percentage" `
                -SampleInterval 1 `
                -MaxSamples 2 `
                -ErrorAction Stop
            $engineRows = @($engines.CounterSamples |
                Where-Object CookedValue -GT 0.01 |
                Group-Object InstanceName |
                ForEach-Object {
                    [pscustomobject]@{
                        Id = $process.Id
                        Engine = $_.Name
                        MaxPercent = [math]::Round((($_.Group | Measure-Object CookedValue -Maximum).Maximum), 2)
                        AveragePercent = [math]::Round((($_.Group | Measure-Object CookedValue -Average).Average), 2)
                    }
                } |
                Sort-Object MaxPercent -Descending)
            if ($engineRows.Count -eq 0) {
                "No active GPU engine was observed for PID $($process.Id) during the sample."
            } else {
                $engineRows | Format-Table -AutoSize
            }
        } catch {
            "GPU counters unavailable for PID $($process.Id): $($_.Exception.Message)"
        }
    }
}

Add-CommandOutput "Communication Health" {
    $config = if (Test-Path -LiteralPath $configPath) {
        Get-Content -LiteralPath $configPath -Raw
    } else {
        ""
    }
    $modeMatch = [regex]::Match($config, '(?m)^startup_mode\s*=\s*"([^"]+)"')
    $mode = if ($modeMatch.Success) { $modeMatch.Groups[1].Value } else { "unknown" }
    $hostMatch = [regex]::Match($config, '(?m)^host\s*=\s*"([^"]+)"')
    $senderHost = if ($hostMatch.Success) { $hostMatch.Groups[1].Value } else { "unknown" }
    $processes = @(Get-Process screen-mirror -ErrorAction SilentlyContinue)
    $udp = if (Get-Command Get-NetUDPEndpoint -ErrorAction SilentlyContinue) {
        @(Get-NetUDPEndpoint -ErrorAction SilentlyContinue |
            Where-Object { $_.LocalPort -in $interestingPorts })
    } else {
        @()
    }
    $tcp = if (Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue) {
        @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
            Where-Object { $_.LocalPort -in $interestingPorts })
    } else {
        @()
    }
    $firewall = if (Get-Command Get-NetFirewallRule -ErrorAction SilentlyContinue) {
        @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction SilentlyContinue |
            Where-Object {
                $_.DisplayName -like "Screen Mirror*" -and
                "$($_.Enabled)" -eq "True" -and
                "$($_.Direction)" -eq "Inbound" -and
                "$($_.Action)" -eq "Allow"
            })
    } else {
        @()
    }

    [pscustomobject]@{
        ConfiguredMode = $mode
        SenderHost = $senderHost
        ProcessCount = $processes.Count
        DiscoveryProbeUdp47776 = @($udp | Where-Object LocalPort -eq 47776).Count -gt 0
        DiscoveryUdp47777 = @($udp | Where-Object LocalPort -eq 47777).Count -gt 0
        TouchUdp47778 = @($udp | Where-Object LocalPort -eq 47778).Count -gt 0
        DiagnosticsTcp47779 = @($tcp | Where-Object LocalPort -eq 47779).Count -gt 0
        ReceiverVideoUdp5004 = @($udp | Where-Object LocalPort -eq 5004).Count -gt 0
        ReceiverAudioUdp5005 = @($udp | Where-Object LocalPort -eq 5005).Count -gt 0
        FirewallInboundAllow = @($firewall).Count -gt 0
    } | Format-List

    $failures = New-Object System.Collections.Generic.List[string]
    if ($processes.Count -eq 0) {
        $failures.Add("screen-mirror.exe is not running.") | Out-Null
    }
    if (@($tcp | Where-Object LocalPort -eq 47779).Count -eq 0) {
        $failures.Add("diagnostics TCP 47779 is not listening.") | Out-Null
    }
    if ($mode -in @("sender", "receiver") -and @($udp | Where-Object LocalPort -eq 47776).Count -eq 0) {
        $failures.Add("active mode is configured but unicast discovery UDP 47776 is not listening.") | Out-Null
    }
    if ($mode -eq "receiver" -and @($udp | Where-Object LocalPort -eq 5004).Count -eq 0) {
        $failures.Add("receiver mode is configured but video UDP 5004 is not listening.") | Out-Null
    }
    if (@($firewall).Count -eq 0) {
        $failures.Add("no enabled Screen Mirror inbound firewall rule was found.") | Out-Null
    }

    if ($failures.Count -eq 0) {
        "Overall: OK"
    } else {
        "Overall: FAIL"
        $failures | ForEach-Object { "- $_" }
    }
}

Add-CommandOutput "Bundled Runtime" {
    $appRoot = Split-Path -Parent $screenMirror
    $requiredFiles = @(
        "glib-2.0-0.dll",
        "gobject-2.0-0.dll",
        "gstreamer-1.0-0.dll",
        "lib\gstreamer-1.0\gstwasapi2.dll",
        "lib\gstreamer-1.0\gstopus.dll",
        "lib\gstreamer-1.0\gstrtp.dll",
        "lib\gstreamer-1.0\gstudp.dll"
    )
    $rows = @($requiredFiles | ForEach-Object {
        $path = Join-Path $appRoot $_
        $item = Get-Item -LiteralPath $path -ErrorAction SilentlyContinue
        [pscustomobject]@{
            File = $_
            Status = if ($item) { "present" } else { "MISSING" }
            Version = if ($item) { $item.VersionInfo.FileVersion } else { $null }
            Bytes = if ($item) { $item.Length } else { $null }
        }
    })
    $rows | Format-Table -AutoSize
    if ($rows.Status -contains "MISSING") {
        "Overall: FAIL - reinstall or repair Screen Mirror."
    } else {
        "Overall: OK"
    }
}

Add-Section "Config"
if (Test-Path -LiteralPath $configPath) {
    $configText = (Get-Content -LiteralPath $configPath -Raw).TrimEnd()
    Add-Line $configText
} else {
    Add-Line "Config file not found."
}

Add-Section "Recent Log"
if (Test-Path -LiteralPath $logPath) {
    Add-Line ((Get-Content -LiteralPath $logPath -Tail 200) -join [Environment]::NewLine)
} else {
    Add-Line "Log file not found."
}

Add-CommandOutput "Update State" {
    if (-not (Test-Path -LiteralPath $updatesPath)) {
        "Update directory not found."
        return
    }

    Get-ChildItem -LiteralPath $updatesPath -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object Name, Length, LastWriteTime |
        Format-Table -AutoSize

    foreach ($name in @("update-attempt.json", "ScreenMirror-update-last-failure.txt")) {
        $path = Join-Path $updatesPath $name
        if (Test-Path -LiteralPath $path) {
            "---- $name ----"
            Get-Content -LiteralPath $path -Raw
        }
    }

    $msiLog = Get-ChildItem -LiteralPath $updatesPath -Filter "ScreenMirror-update-v*.log" -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if ($msiLog) {
        "---- $($msiLog.Name) relevant failures ----"
        $matches = Get-Content -LiteralPath $msiLog.FullName |
            Select-String -Pattern "Return value 3|error 1[0-9]{3}|MainEngineThread|Installation success or error status" |
            Select-Object -Last 80
        if ($matches) {
            $matches | ForEach-Object { $_.Line }
        } else {
            "No MSI failure markers found."
        }
        "---- $($msiLog.Name) tail ----"
        Get-Content -LiteralPath $msiLog.FullName -Tail 80
    }
}

Add-CommandOutput "Bundled VDD Devices" {
    $devices = Get-BundledVddDevices
    if ($devices.Count -eq 0) {
        "none"
    } else {
        $devices | Select-Object Status, Class, FriendlyName, InstanceId | Format-Table -AutoSize
    }
}

Add-CommandOutput "Bundled VDD Monitors" {
    $devices = Get-BundledVddMonitors
    if ($devices.Count -eq 0) {
        "none"
    } else {
        $devices | Select-Object Status, Class, FriendlyName, InstanceId | Format-Table -AutoSize
    }
}

Add-CommandOutput "Other Virtual Display Candidates" {
    $devices = Get-VirtualDisplayCandidates
    if ($devices.Count -eq 0) {
        "none"
    } else {
        $devices | Select-Object Status, Class, FriendlyName, InstanceId | Format-Table -AutoSize
    }
}

Add-CommandOutput "Windows Display Devices" {
    Invoke-ScreenMirror @("monitors")
}

Add-CommandOutput "Windows Monitor Handles" {
    Get-MonitorHandles | Format-Table -AutoSize
}

Add-CommandOutput "GStreamer Probe" {
    Invoke-ScreenMirror @("probe")
}

Add-CommandOutput "GStreamer D3D11 Screen Capture Source" {
    Invoke-GstInspect @("d3d11screencapturesrc")
}

Add-CommandOutput "Receiver Discovery" {
    Invoke-ScreenMirror @("discover", "--timeout-ms", $DiscoveryTimeoutMs, "--pin", $pin)
}

Add-CommandOutput "UDP Endpoints" {
    if (Get-Command Get-NetUDPEndpoint -ErrorAction SilentlyContinue) {
        $endpoints = @(Get-NetUDPEndpoint -ErrorAction SilentlyContinue |
            Where-Object { $_.LocalPort -in $interestingPorts } |
            Select-Object LocalAddress, LocalPort, OwningProcess)
        if ($endpoints.Count -eq 0) {
            "none"
        } else {
            $endpoints | Format-Table -AutoSize
        }
    } else {
        netstat -ano -p udp | Select-String -Pattern ($interestingPorts -join "|")
    }
}

Add-CommandOutput "TCP Endpoints" {
    if (Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue) {
        $endpoints = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
            Where-Object { $_.LocalPort -in $interestingPorts } |
            Select-Object LocalAddress, LocalPort, OwningProcess, State)
        if ($endpoints.Count -eq 0) {
            "none"
        } else {
            $endpoints | Format-Table -AutoSize
        }
    } else {
        netstat -ano -p tcp | Select-String -Pattern ($interestingPorts -join "|")
    }
}

Add-CommandOutput "Network Profiles" {
    if (Get-Command Get-NetConnectionProfile -ErrorAction SilentlyContinue) {
        Get-NetConnectionProfile -ErrorAction SilentlyContinue |
            Select-Object InterfaceAlias, Name, NetworkCategory, IPv4Connectivity, IPv6Connectivity |
            Format-Table -AutoSize
    } else {
        netsh advfirewall show currentprofile
    }
}

Add-CommandOutput "Network Adapters" {
    if (Get-Command Get-NetIPConfiguration -ErrorAction SilentlyContinue) {
        Get-NetIPConfiguration |
            Select-Object InterfaceAlias, InterfaceDescription, IPv4Address, IPv4DefaultGateway, DNSServer |
            Format-List
    } else {
        ipconfig /all
    }
}

Add-CommandOutput "Firewall Rules" {
    if (Get-Command Get-NetFirewallRule -ErrorAction SilentlyContinue) {
        $rules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -like "Screen Mirror*" })
        if ($rules.Count -eq 0) {
            "Overall: FAIL - no Screen Mirror firewall rule is installed."
            "Expected: enabled inbound allow rule for the Screen Mirror executable on the local subnet."
            return
        }

        $rows = foreach ($rule in $rules) {
            $application = $rule | Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue
            $port = $rule | Get-NetFirewallPortFilter -ErrorAction SilentlyContinue
            $address = $rule | Get-NetFirewallAddressFilter -ErrorAction SilentlyContinue
            [pscustomobject]@{
                DisplayName = $rule.DisplayName
                Enabled = $rule.Enabled
                Direction = $rule.Direction
                Action = $rule.Action
                Profile = $rule.Profile
                Program = $application.Program
                Protocol = $port.Protocol
                LocalPort = $port.LocalPort
                RemoteAddress = $address.RemoteAddress
            }
        }
        $rows | Format-List

        $usable = @($rules | Where-Object {
            $_.Enabled -eq "True" -and $_.Direction -eq "Inbound" -and $_.Action -eq "Allow"
        })
        if ($usable.Count -eq 0) {
            "Overall: FAIL - Screen Mirror has no enabled inbound allow rule."
        } else {
            "Overall: OK - Screen Mirror has an enabled inbound allow rule."
        }
    } else {
        netsh advfirewall show currentprofile
    }
}

$report = $lines -join [Environment]::NewLine
$report | Set-Content -LiteralPath $reportPath -Encoding UTF8

try {
    if (-not $NoClipboard) {
        Set-Clipboard -Value $report
        Add-Content -LiteralPath $reportPath -Value ([Environment]::NewLine + "==== Clipboard ====" + [Environment]::NewLine + "Copied diagnostics report to clipboard.") -Encoding UTF8
    }
} catch {
    Add-Content -LiteralPath $reportPath -Value ([Environment]::NewLine + "==== Clipboard ====" + [Environment]::NewLine + "Failed to copy diagnostics report to clipboard: $($_.Exception.Message)") -Encoding UTF8
}

if ($Stdout) {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    [Console]::Out.Write($report)
}

if (-not $NoNotepad) {
    Start-Process -FilePath "notepad.exe" -ArgumentList "`"$reportPath`"" | Out-Null
}
