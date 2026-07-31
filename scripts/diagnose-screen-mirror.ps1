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
$sampledProcessResources = @()
$sampledGpuMemory = @()
$sampledGpuEngines = @()
$gpuEngineSampleSucceeded = $false

function Add-Line([string] $Line = "") {
    $script:lines.Add($Line) | Out-Null
}

# Every log line the application writes begins with this stamp; builds before it wrote the bare
# message.
$script:logStampPattern = '^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} '

# The same anchor for the few places that keep whole lines because they print the timestamp.
$script:logMessageAnchor = '^(?:\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} )?'

# The log tail with the timestamp stripped off each line.
#
# Every verdict in this script anchors its patterns with ^ against the message, so reading the file
# raw made all of them blind to any line a current build wrote - they could only ever match the
# pre-timestamp lines still sitting in the tail. A real report claimed the last sender route was a
# zero-copy NVIDIA one while the log's newest route, six minutes newer and timestamped, had fallen
# back to system memory. Stripping the stamp once, here, is what makes a pattern mean the same thing
# whichever build wrote the line. The Recent Log section still prints the file verbatim, which is
# where the question of *when* something happened is answered.
function Get-LogMessageTail([int] $Tail) {
    if (-not (Test-Path -LiteralPath $logPath)) {
        return @()
    }
    return @(Get-Content -LiteralPath $logPath -Tail $Tail -ErrorAction SilentlyContinue |
        ForEach-Object { $_ -replace $script:logStampPattern, "" })
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

function Get-ConfigSectionValue([string] $Config, [string] $Section, [string] $Key) {
    if ([string]::IsNullOrWhiteSpace($Config)) {
        return $null
    }

    $inSection = $false
    $valuePattern = '^\s*' + [regex]::Escape($Key) + '\s*=\s*([^#\r\n]+)'
    foreach ($line in ($Config -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[([^\]]+)\]\s*$') {
            $inSection = $Matches[1] -eq $Section
            continue
        }
        if ($inSection -and $line -match $valuePattern) {
            return $Matches[1].Trim().Trim('"').Trim("'")
        }
    }
    return $null
}

function Get-RedactedConfigText([string] $Config) {
    # Diagnostics still need the surrounding configuration, but the pairing PIN must never
    # leave this machine in the report, clipboard, or Notepad copy.
    $pinPattern = '(?m)^(\s*pin\s*=\s*)(?:"[^"]*"|''[^'']*''|[^\s#\r\n]+)(\s*(?:#.*)?)$'
    return $Config -replace $pinPattern, '$1"<redacted>"$2'
}

function Get-SenderVirtualDisplayVerdict {
    $config = if (Test-Path -LiteralPath $configPath) {
        Get-Content -LiteralPath $configPath -Raw -ErrorAction SilentlyContinue
    } else {
        ""
    }
    $enableValue = Get-ConfigSectionValue $config "send" "enable_virtual_display"
    $preferValue = Get-ConfigSectionValue $config "send" "prefer_virtual_display"
    $monitorIndex = Get-ConfigSectionValue $config "send" "monitor_index"
    # These sender options default to true/-1 in the application when omitted.
    $vddEnabled = $enableValue -ne "false"
    $preferVdd = $preferValue -ne "false"
    if ([string]::IsNullOrWhiteSpace($monitorIndex)) {
        $monitorIndex = "-1"
    }

    $logLines = Get-LogMessageTail 1000
    $senderLogLines = $logLines
    $lastSupervisorStart = -1
    for ($index = 0; $index -lt $logLines.Count; $index++) {
        if ($logLines[$index] -match '^sender supervisor started;') {
            $lastSupervisorStart = $index
        }
    }
    if ($lastSupervisorStart -ge 0) {
        $senderLogLines = @($logLines[$lastSupervisorStart..($logLines.Count - 1)])
    }
    $lastSenderSource = $senderLogLines |
        Where-Object { $_ -match '^sender pipeline source:' } |
        Select-Object -Last 1

    $issuePatterns = @(
        @{ Name = "VDD was not capture-ready before sender start"; Pattern = 'bundled VDD was not capture-ready before sender start' },
        @{ Name = "sender fell back because no bundled VDD capture target was ready"; Pattern = 'no bundled virtual display is capture-ready; receivers fall back to the default capture target' },
        @{ Name = "sender could not find its preferred virtual display"; Pattern = 'sender preferred virtual display: not found' },
        @{ Name = "sender blocked physical-display fallback because its VDD target was missing"; Pattern = 'refusing physical-display fallback|no capture-ready virtual target was found' },
        @{ Name = "VDD capture-target count did not match the receiver count"; Pattern = 'requested \d+ virtual displays but (?:only )?\d+ are capture-ready' },
        @{ Name = "receiver mode sync failed"; Pattern = 'virtual display (?:resolution )?sync .*failed:|failed to sync virtual display|did not apply .* within \d+ms' },
        @{ Name = "receiver mode is unsupported by the VDD"; Pattern = 'virtual display .* does not support .*(?:keeping current mode|DISP_CHANGE_BADMODE)|virtual display .* kept its current mode:' },
        @{ Name = "a detached VDD endpoint could not be attached to the desktop"; Pattern = 'bundled VDD .* attach failed:|bundled VDD attach commit failed:' },
        @{ Name = "VDD stayed detached after both extend and direct attach"; Pattern = 'bundled VDD is still not capture-ready' },
        @{ Name = "sender start failed repeatedly and was reported to the tray"; Pattern = 'sender environment still not ready' }
    )
    $recentIssues = New-Object System.Collections.Generic.List[string]
    foreach ($issue in $issuePatterns) {
        if (@($senderLogLines | Where-Object { $_ -match $issue.Pattern }).Count -gt 0) {
            $recentIssues.Add($issue.Name) | Out-Null
        }
    }

    # With the default monitor index, a sender source that contains no monitor handle is the
    # physical/default-display branch. The virtual target branch always records a handle.
    if ($vddEnabled -and $preferVdd -and $monitorIndex -eq "-1" -and $lastSenderSource -and
        $lastSenderSource -match 'monitor-index=' -and $lastSenderSource -notmatch 'monitor-handle=') {
        $recentIssues.Add("last sender pipeline used a monitor index instead of a VDD monitor handle") | Out-Null
    }

    $monitorOutput = Invoke-ScreenMirror @("monitors")
    $monitorQueryAvailable = -not [string]::IsNullOrWhiteSpace($monitorOutput) -and
        $monitorOutput -notmatch '^screen-mirror\.exe not found$'
    $monitorLines = if ($monitorQueryAvailable) {
        @($monitorOutput -split "`r?`n")
    } else {
        @()
    }
    # Current releases print the structured `capture-index=Some(...) bundled-vdd=true`
    # summary. Older installed builds use the concise `[index] ... bundled-vdd` form;
    # accept both so the diagnostic remains useful before a local update.
    $bundledLines = @($monitorLines | Where-Object { $_ -match 'bundled-vdd(?:=true|(?:\s|$))' })
    $captureReadyLines = @($bundledLines | Where-Object {
        $_ -match 'capture-index=Some\(\d+\)' -or
        $_ -match '^\[\d+\].*bundled-vdd(?:=true|(?:\s|$))'
    })
    $vddDevices = @(Get-BundledVddDevices)
    $vddMonitors = @(Get-BundledVddMonitors)
    # Each driver restart hands the desktop a fresh generation of monitor children, and the previous
    # generation stays in the registry as a node Windows lists but cannot see. The application prunes
    # these itself now; counting them here is how that pruning is observable, and a count that keeps
    # climbing across sessions means it is not running or not permitted to.
    $staleVddMonitors = @($vddMonitors | Where-Object { $_.Status -ne "OK" })
    $lastPrune = $logLines |
        Where-Object { $_ -match '^stale virtual display monitor nodes (?:pruned|could not be pruned)' } |
        Select-Object -Last 1
    # A bundled endpoint Windows left off the desktop is the state direct attach exists to repair,
    # so report it separately from "no endpoint at all".
    $detachedLines = @($bundledLines | Where-Object { $_ -match 'attached=false' -or $_ -match '\sdetached(?:\s|$)' })
    $attachLine = $senderLogLines |
        Where-Object { $_ -match '^bundled VDD .* (?:attached at x=|attach failed:|attach skipped:)' } |
        Select-Object -Last 1
    $prepareFailures = @($senderLogLines |
        Where-Object { $_ -match '^sender environment not ready;' }).Count
    $lastPrepareFailure = $senderLogLines |
        Where-Object { $_ -match '^sender environment not ready;' } |
        Select-Object -Last 1

    $verdict = if (-not $vddEnabled) {
        "NOT CONFIGURED - sender virtual-display setup is disabled."
    } elseif ($recentIssues.Count -gt 0) {
        "FAIL - configured sender VDD fallback or readiness issue was recorded in the recent log."
    } elseif (-not $monitorQueryAvailable) {
        "UNKNOWN - current VDD capture targets could not be queried."
    } elseif ($captureReadyLines.Count -eq 0 -and $detachedLines.Count -gt 0) {
        "FAIL - a bundled virtual display exists but is detached from the desktop; the next sender start attaches it directly, and Win+P Extend does the same by hand."
    } elseif ($captureReadyLines.Count -eq 0) {
        "FAIL - sender VDD is configured, but no bundled virtual display is currently capture-ready."
    } else {
        "OK - configured sender VDD has a current capture-ready target."
    }

    [pscustomobject]@{
        Verdict = $verdict
        EnableVirtualDisplay = if ($null -eq $enableValue) { "true (default)" } else { $enableValue }
        PreferVirtualDisplay = if ($null -eq $preferValue) { "true (default)" } else { $preferValue }
        MonitorIndex = $monitorIndex
        CurrentBundledVddTargets = if ($monitorQueryAvailable) { $bundledLines.Count } else { "UNKNOWN" }
        CurrentCaptureReadyTargets = if ($monitorQueryAvailable) { $captureReadyLines.Count } else { "UNKNOWN" }
        BundledVddDeviceNodes = $vddDevices.Count
        BundledVddMonitorNodes = $vddMonitors.Count
        StaleBundledVddMonitorNodes = $staleVddMonitors.Count
        LastMonitorNodePrune = if ($lastPrune) { $lastPrune } else { "(none in last 1000 log lines)" }
        DetachedBundledVddTargets = if ($monitorQueryAvailable) { $detachedLines.Count } else { "UNKNOWN" }
        LastDesktopAttachAttempt = if ($attachLine) { $attachLine } else { "(none in latest sender session)" }
        SenderStartFailures = $prepareFailures
        LastSenderStartFailure = if ($lastPrepareFailure) { $lastPrepareFailure } else { "(none in latest sender session)" }
        LastSenderCaptureSource = if ($lastSenderSource) { $lastSenderSource } else { "(not recorded)" }
        RecentVddWarnings = if ($recentIssues.Count -gt 0) { $recentIssues -join "; " } else { "(none in latest sender session / last 1000 log lines)" }
    }
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

function Test-VideoEncodeGpuEngine([string] $Engine) {
    if ([string]::IsNullOrWhiteSpace($Engine)) {
        return $false
    }

    return $Engine -match "(?i)(?:engtype_)?video[_ ]?encode(?:[_ ]|$)"
}

function Test-VideoCodecGpuEngine([string] $Engine) {
    if ([string]::IsNullOrWhiteSpace($Engine)) {
        return $false
    }

    # Some Radeon drivers expose a combined codec engine rather than separate
    # encode/decode counters. Keep it separate so receiver work is not labelled
    # as encoding (or sender work as decoding).
    return $Engine -match "(?i)(?:engtype_)?video[_ ]?codec(?:[_ ]|$)"
}

function Test-VideoDecodeGpuEngine([string] $Engine) {
    if ([string]::IsNullOrWhiteSpace($Engine)) {
        return $false
    }

    # Intel and NVIDIA expose the hardware decoder as "Video Decode".  Do not
    # treat AMD's combined "Video Codec" counter as decode here: it can contain
    # either AMF encode or decode work, so reporting it as decode would mislead.
    return $Engine -match "(?i)(?:engtype_)?video[_ ]?decode(?:[_ ]|$)"
}

function Get-ReceiverPlaybackRoute {
    $logLines = Get-LogMessageTail 1000
    $adapterLine = $logLines |
        Where-Object { $_ -match "^receiver GPU selected:" } |
        Select-Object -Last 1
    $routeLine = $logLines |
        Where-Object {
            $_ -match "^receiver profile=\S+\s+adapter=.*\s+decoder=\S+(?:\s+decoder-luid=\S+)?\s+memory=\S+\s+sink=" -or
            $_ -match "^receiver decoder=\S+\s+sink="
        } |
        Select-Object -Last 1
    $runtimeLine = $logLines |
        Where-Object {
            $_ -match "^receiver runtime decoder=\S+(?:\s+decoder-luid=\S+)?\s+memory=.*\s+caps=.*\s+sink=\S+(?:\s+sink-adapter=\S+)?$"
        } |
        Select-Object -Last 1
    $pipelineLine = $logLines |
        Where-Object { $_ -match "^receiver video pipeline:" } |
        Select-Object -Last 1
    # Regex.Match rejects a null input.  A missing route is normal on a fresh
    # install or after log rotation, so turn it into an empty string first.
    $routeText = if ($null -eq $routeLine) { "" } else { $routeLine.ToString() }
    $plannedMatch = [regex]::Match(
        $routeText,
        "^receiver profile=(\S+)\s+adapter=(.*?)\s+decoder=(\S+)(?:\s+decoder-luid=(\S+))?\s+memory=(\S+)\s+sink=(.+)$"
    )
    $legacyMatch = [regex]::Match($routeText, "^receiver decoder=(\S+)\s+sink=(.+)$")
    $runtimeText = if ($null -eq $runtimeLine) { "" } else { $runtimeLine.ToString() }
    $runtimeMatch = [regex]::Match(
        $runtimeText,
        "^receiver runtime decoder=(\S+)(?:\s+decoder-luid=(\S+))?\s+memory=(.*?)\s+caps=(.*?)\s+sink=(\S+)(?:\s+sink-adapter=(\S+))?$"
    )

    $profile = if ($plannedMatch.Success) { $plannedMatch.Groups[1].Value } else { "UNKNOWN" }
    $adapter = if ($plannedMatch.Success) {
        $plannedMatch.Groups[2].Value
    } elseif ($adapterLine) {
        $adapterLine -replace "^receiver GPU selected:\s*", ""
    } else {
        "(not recorded; auto selection may be in use)"
    }
    $configuredDecoder = if ($plannedMatch.Success) {
        $plannedMatch.Groups[3].Value
    } elseif ($legacyMatch.Success) {
        $legacyMatch.Groups[1].Value
    } else {
        "UNKNOWN"
    }
    $decoder = if ($runtimeMatch.Success) {
        $runtimeMatch.Groups[1].Value
    } else {
        $configuredDecoder
    }
    $configuredMemory = if ($plannedMatch.Success) {
        $plannedMatch.Groups[5].Value
    } else {
        "UNKNOWN"
    }
    $memoryPath = if ($runtimeMatch.Success) {
        $runtimeMatch.Groups[3].Value
    } elseif ($configuredMemory -ne "UNKNOWN") {
        "$configuredMemory (planned; no negotiated caps were logged)"
    } elseif ($decoder -match "^d3d12" -and $routeText -match "sink=d3d12videosink") {
        "D3D12Memory expected between hardware decoder and D3D12 sink"
    } elseif ($decoder -match "^d3d11" -and $routeText -match "sink=d3d11videosink") {
        "D3D11Memory expected between hardware decoder and D3D11 sink"
    } else {
        "UNKNOWN - no negotiated decoder caps were found in the log"
    }
    $negotiatedCaps = if ($runtimeMatch.Success) {
        $runtimeMatch.Groups[4].Value
    } else {
        "(not recorded; run diagnostics while video is being received)"
    }
    $sink = if ($runtimeMatch.Success) {
        $runtimeMatch.Groups[5].Value
    } elseif ($plannedMatch.Success) {
        $plannedMatch.Groups[6].Value
    } elseif ($legacyMatch.Success) {
        $legacyMatch.Groups[2].Value
    } else {
        "UNKNOWN"
    }
    $decoderLuid = if ($runtimeMatch.Success -and $runtimeMatch.Groups[2].Value) {
        $runtimeMatch.Groups[2].Value
    } elseif ($plannedMatch.Success -and $plannedMatch.Groups[4].Value) {
        $plannedMatch.Groups[4].Value
    } else {
        "default/unknown"
    }
    $sinkAdapter = if ($runtimeMatch.Success -and $runtimeMatch.Groups[6].Value) {
        $runtimeMatch.Groups[6].Value
    } elseif ($sink -match "(?:^|\s)adapter=(\d+)") {
        $Matches[1]
    } else {
        "default/unknown"
    }

    # A hardware decoder rejects a stream outside its DXVA caps before the first frame reaches it,
    # so the receiver walks its routes until one plays.  Report which retry actually took over.
    # Only a retry logged after the most recent planned route belongs to the current session.
    $planIndex = -1
    $fallbackIndex = -1
    $limitIndex = -1
    for ($i = 0; $i -lt $logLines.Count; $i++) {
        if ($logLines[$i] -match "^receiver profile=\S+\s+adapter=") { $planIndex = $i }
        if ($logLines[$i] -match "^receiver route failed; retrying on the (\S+) route:") { $fallbackIndex = $i }
        if ($logLines[$i] -match "^receiver decoder \S+ (?:accepts up to|advertises no frame-size limit)") { $limitIndex = $i }
    }
    $fallbackLine = if ($fallbackIndex -gt $planIndex) { $logLines[$fallbackIndex] } else { $null }
    # The limit this receiver announces to senders, which a sender scales its capture down to
    # instead of pushing a stream this decoder would reject. It is logged right after the route it
    # belongs to, so an older one is not this session's.
    $decodeLimitLine = if ($limitIndex -gt $planIndex) { $logLines[$limitIndex] } else { $null }
    $decodeLimit = if ($decodeLimitLine -match "accepts up to (\d+x\d+)") {
        "$($Matches[1]) announced to senders"
    } elseif ($decodeLimitLine) {
        "NONE - this decoder advertises no frame-size limit, so senders will not scale for it"
    } else {
        "(not recorded)"
    }
    $fallbackNote = if ($fallbackLine) {
        $fallbackLine -replace "^receiver route failed; retrying on the ", "retried on the " -replace "\s+$", ""
    } else {
        "(none; the primary route played)"
    }

    $hardwareProfile = if ($profile -ne "UNKNOWN") {
        $profile
    } elseif ($decoder -match "^d3d12.*h264.*dec$") {
        "D3D12/DXVA H.264 hardware decode requested"
    } elseif ($decoder -match "^d3d11.*h264.*dec$") {
        "D3D11/DXVA H.264 hardware decode requested"
    } elseif ($decoder -eq "decodebin") {
        "Autoplug decode (hardware decoder selection is not recorded)"
    } elseif ($decoder -match "^(?:avdec|openh264)") {
        "Software H.264 decode"
    } elseif ($decoder -eq "UNKNOWN") {
        "UNKNOWN - no receiver route was found in the log"
    } else {
        "Decoder requested: $decoder"
    }

    [pscustomobject]@{
        HardwareProfile = $hardwareProfile
        Adapter = $adapter
        ConfiguredDecoder = $configuredDecoder
        ActualDecoder = $decoder
        DecoderAdapterLuid = $decoderLuid
        MemoryPath = $memoryPath
        NegotiatedCaps = $negotiatedCaps
        Sink = $sink
        SinkAdapterIndex = $sinkAdapter
        DecoderFrameLimit = $decodeLimit
        RouteFallback = $fallbackNote
        LastRuntimeRoute = if ($runtimeLine) { $runtimeLine } else { "(not recorded)" }
        LastPipeline = if ($pipelineLine) { $pipelineLine } else { "(not recorded)" }
    }
}

function Get-GpuAccelerationVerdict {
    $adapterNames = @()
    try {
        $adapterNames = @(Get-CimInstance Win32_VideoController -ErrorAction Stop |
            ForEach-Object { $_.Name } |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    } catch {
        $adapterNames = @()
    }
    $radeonAdapters = @($adapterNames | Where-Object { $_ -match "(?i)\b(AMD|ATI|Radeon)\b" })

    $probe = Invoke-ScreenMirror @("probe")
    $amfFactory = if ($probe -match "(?im)^amf encode\s+amfh264enc\s+yes\s*$") {
        "AVAILABLE"
    } elseif ($probe -match "(?im)^amf encode\s+amfh264enc\s+no\s*$") {
        "UNAVAILABLE"
    } else {
        $amfInspect = Invoke-GstInspect @("amfh264enc")
        if ($amfInspect -match "(?im)^Factory Details:\s*$") {
            "AVAILABLE"
        } elseif ($amfInspect -match "(?i)No such element or plugin") {
            "UNAVAILABLE"
        } else {
            "UNKNOWN - current executable does not report AMF and gst-inspect could not confirm it"
        }
    }

    $logLines = Get-LogMessageTail 1000
    # A capture-GPU mismatch belongs to one pipeline start, so it is only reported when it was
    # logged for the newest route; an older mismatch would otherwise keep flagging a route that
    # has since been fixed.
    $routeIndex = -1
    $previousRouteIndex = -1
    $mismatchIndex = -1
    $failureIndex = -1
    $droppedIndex = -1
    $clampIndex = -1
    for ($index = 0; $index -lt $logLines.Count; $index++) {
        $line = $logLines[$index]
        if ($line -match "^sender encoder=\S+\s+frame-memory=\S+") {
            $previousRouteIndex = $routeIndex
            $routeIndex = $index
        } elseif ($line -match "^sender encoder=.*does not run on the capture GPU") {
            $mismatchIndex = $index
        } elseif ($line -match "^sender video pipeline stopped:") {
            $failureIndex = $index
        } elseif ($line -match "^encoder \S+ does not expose ") {
            $droppedIndex = $index
        } elseif ($line -match "^receiver \S+ decodes up to ") {
            $clampIndex = $index
        }
    }
    $routeLine = if ($routeIndex -ge 0) { $logLines[$routeIndex] } else { $null }
    $captureGpuMismatchLine = if ($mismatchIndex -gt $previousRouteIndex -and $mismatchIndex -ge 0) {
        $logLines[$mismatchIndex]
    } else {
        $null
    }
    $gpuLine = $logLines |
        Where-Object { $_ -match "^sender GPU selected:" } |
        Select-Object -Last 1
    $autoGpuLine = $logLines |
        Where-Object { $_ -match "^sender automatic GPU selected from the capture display:" } |
        Select-Object -Last 1
    # A pipeline failure and any dropped encoder settings belong to the route that logged them, so
    # both are reported only while the newest route is still the one that failed. An update or a
    # config change that fixed the route otherwise keeps showing its last failure forever.
    $pipelineFailureLine = if ($failureIndex -gt $routeIndex) { $logLines[$failureIndex] } else { $null }
    $droppedSettingsLine = if ($droppedIndex -gt $previousRouteIndex -and $droppedIndex -ge 0) {
        $logLines[$droppedIndex]
    } else {
        $null
    }
    # The scale-to-fit decision is logged while the route is being built, so it belongs to the
    # newest route the same way dropped encoder settings do.
    $decodeClampLine = if ($clampIndex -gt $previousRouteIndex -and $clampIndex -ge 0) {
        $logLines[$clampIndex]
    } else {
        $null
    }
    $routeMatch = [regex]::Match([string] $routeLine, "sender encoder=(\S+)\s+frame-memory=(\S+)")
    $encoder = if ($routeMatch.Success) { $routeMatch.Groups[1].Value } else { "UNKNOWN" }
    $frameMemory = if ($routeMatch.Success) { $routeMatch.Groups[2].Value } else { "UNKNOWN" }

    $amfStatus = if ($radeonAdapters.Count -eq 0) {
        "NOT APPLICABLE - no Radeon adapter detected"
    } elseif ($encoder -match "(?i)^amfh264") {
        "OK - active in the last sender route"
    } elseif ($encoder -ne "UNKNOWN") {
        "NOT ACTIVE - last sender encoder was $encoder"
    } elseif ($amfFactory -eq "AVAILABLE") {
        "AVAILABLE - no sender route was found in the log"
    } else {
        "NOT ACTIVE - AMF factory is $amfFactory"
    }

    $zeroCopyStatus = switch ($frameMemory.ToUpperInvariant()) {
        "D3D11" { "OK - D3D11 zero-copy active in the last sender route" }
        "SYSTEM" { "FALLBACK - last sender route downloaded frames to system memory" }
        default { "UNKNOWN - no sender route was found in the log" }
    }

    $videoEncodeRows = @($script:sampledGpuEngines |
        Where-Object { Test-VideoEncodeGpuEngine $_.Engine })
    if ($encoder -match "(?i)^amfh264") {
        $videoEncodeRows += @($script:sampledGpuEngines |
            Where-Object { Test-VideoCodecGpuEngine $_.Engine })
    }
    $running = @(Get-Process screen-mirror -ErrorAction SilentlyContinue).Count -gt 0
    if ($videoEncodeRows.Count -gt 0) {
        $maxVideoEncode = ($videoEncodeRows | Measure-Object MaxPercent -Maximum).Maximum
        $videoEncodeStatus = "OBSERVED - maximum ${maxVideoEncode}% during the sample"
    } elseif ($script:gpuEngineSampleSucceeded) {
        $videoEncodeStatus = "NOT OBSERVED - run diagnostics while the sender is actively mirroring; a light workload can also read near zero"
    } elseif ($running) {
        $videoEncodeStatus = "UNAVAILABLE - GPU engine counters could not be sampled"
    } else {
        $videoEncodeStatus = "NOT SAMPLED - screen-mirror.exe was not running"
    }

    $fallbackLine = if ($frameMemory -eq "system") {
        $logLines |
            Where-Object { $_ -match "^sender encoder=.*falling back to system-memory frames$" } |
            Select-Object -Last 1
    } else {
        $null
    }

    [pscustomobject]@{
        RadeonAdapter = if ($radeonAdapters.Count -gt 0) { $radeonAdapters -join "; " } else { "NOT DETECTED" }
        AmfFactory = $amfFactory
        RadeonAmf = $amfStatus
        D3D11ZeroCopy = $zeroCopyStatus
        VideoEncodeEngine = $videoEncodeStatus
        LastSenderGpu = if ($gpuLine) { $gpuLine } else { "(not recorded; auto selection may be in use)" }
        AutomaticGpuFromCaptureDisplay = if ($autoGpuLine) { $autoGpuLine } else { "(not recorded; an explicit gpu= may be configured)" }
        EncoderOnCaptureGpu = if ($captureGpuMismatchLine) {
            "MISMATCH - $captureGpuMismatchLine"
        } elseif ($frameMemory -eq "D3D11") {
            "OK - encoder and capture share one adapter"
        } else {
            "UNKNOWN - no capture-GPU mismatch was recorded in the log tail"
        }
        LastSenderRoute = if ($routeLine) { $routeLine } else { "(not recorded)" }
        ReceiverDecodeClamp = if ($decodeClampLine) {
            $decodeClampLine
        } else {
            "(none; the capture fits every receiver's decoder or none announced a limit)"
        }
        LastPipelineFailure = if ($pipelineFailureLine) { $pipelineFailureLine } else { "(none in the log tail)" }
        DroppedEncoderSettings = if ($droppedSettingsLine) { $droppedSettingsLine } else { "(none in the log tail)" }
        LatestFallbackReason = if ($fallbackLine) {
            $fallbackLine
        } elseif ($frameMemory -eq "system") {
            "(not found in the log tail)"
        } else {
            "(not applicable to the last route)"
        }
    }
}

function Get-ScreenMirrorResourceSummary {
    $resourceRows = @($script:sampledProcessResources)
    $gpuMemoryRows = @($script:sampledGpuMemory)
    $gpuEngineRows = @($script:sampledGpuEngines)
    $videoEncodeRows = @($gpuEngineRows |
        Where-Object { Test-VideoEncodeGpuEngine $_.Engine })
    $videoDecodeRows = @($gpuEngineRows |
        Where-Object { Test-VideoDecodeGpuEngine $_.Engine })
    $videoCodecRows = @($gpuEngineRows |
        Where-Object { Test-VideoCodecGpuEngine $_.Engine })

    if ($resourceRows.Count -eq 0) {
        return [pscustomobject]@{
            Status = "NOT SAMPLED - screen-mirror.exe was not running"
            ProcessCount = 0
            CpuPercentTotal = "N/A"
            WorkingSetMiB = "N/A"
            PrivateMiB = "N/A"
            GpuDedicatedMiB = "N/A"
            GpuSharedMiB = "N/A"
            GpuEnginePeakPercent = "N/A"
            GpuEngineCurrentPercent = "N/A"
            VideoEncodePeakPercent = "N/A"
            VideoDecodePeakPercent = "N/A"
            VideoDecodeCurrentPercent = "N/A"
            VideoCodecPeakPercent = "N/A"
            VideoCodecCurrentPercent = "N/A"
            GpuCounterInterpretation = "N/A"
        }
    }

    $cpuTotal = ($resourceRows | Measure-Object CpuPercent -Sum).Sum
    $workingSetTotal = ($resourceRows | Measure-Object WorkingSetMiB -Sum).Sum
    $privateTotal = ($resourceRows | Measure-Object PrivateMiB -Sum).Sum
    $gpuDedicated = if ($gpuMemoryRows.Count -gt 0) {
        [math]::Round(($gpuMemoryRows | Measure-Object GpuDedicatedMiB -Sum).Sum, 1)
    } else {
        "UNAVAILABLE"
    }
    $gpuShared = if ($gpuMemoryRows.Count -gt 0) {
        [math]::Round(($gpuMemoryRows | Measure-Object GpuSharedMiB -Sum).Sum, 1)
    } else {
        "UNAVAILABLE"
    }
    $gpuPeak = if ($script:gpuEngineSampleSucceeded) {
        if ($gpuEngineRows.Count -gt 0) {
            [math]::Round(($gpuEngineRows | Measure-Object MaxPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $videoEncodePeak = if ($script:gpuEngineSampleSucceeded) {
        if ($videoEncodeRows.Count -gt 0) {
            [math]::Round(($videoEncodeRows | Measure-Object MaxPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $gpuCurrent = if ($script:gpuEngineSampleSucceeded) {
        if ($gpuEngineRows.Count -gt 0) {
            [math]::Round(($gpuEngineRows | Measure-Object CurrentPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $videoDecodePeak = if ($script:gpuEngineSampleSucceeded) {
        if ($videoDecodeRows.Count -gt 0) {
            [math]::Round(($videoDecodeRows | Measure-Object MaxPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $videoDecodeCurrent = if ($script:gpuEngineSampleSucceeded) {
        if ($videoDecodeRows.Count -gt 0) {
            [math]::Round(($videoDecodeRows | Measure-Object CurrentPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $videoCodecPeak = if ($script:gpuEngineSampleSucceeded) {
        if ($videoCodecRows.Count -gt 0) {
            [math]::Round(($videoCodecRows | Measure-Object MaxPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $videoCodecCurrent = if ($script:gpuEngineSampleSucceeded) {
        if ($videoCodecRows.Count -gt 0) {
            [math]::Round(($videoCodecRows | Measure-Object CurrentPercent -Maximum).Maximum, 2)
        } else {
            0
        }
    } else {
        "UNAVAILABLE"
    }
    $latestRuntimeRoute = Get-LogMessageTail 1000 |
        Where-Object { $_ -match "^receiver runtime decoder=" } |
        Select-Object -Last 1
    $gpuCounterInterpretation = if (
        $script:gpuEngineSampleSucceeded -and
        $gpuPeak -eq 0 -and
        $latestRuntimeRoute -match "^receiver runtime decoder=(?:d3d12|d3d11|nvh264|qsvh264|mfh264)\S*(?:\s+decoder-luid=\S+)?\s+memory=D3D1[12]Memory\s"
    ) {
        "D3D hardware decode and GPU-memory caps were confirmed; this Windows/driver counter set reported 0%."
    } elseif (-not $script:gpuEngineSampleSucceeded) {
        "Windows GPU performance counters were unavailable; use Receiver Playback Route to confirm hardware decode."
    } else {
        "GPU percentages are Windows performance-counter samples; Receiver Playback Route reports the negotiated decoder memory path."
    }

    [pscustomobject]@{
        Status = "SAMPLED - CPU over 1 second; GPU engines over 5 seconds per process"
        ProcessCount = $resourceRows.Count
        CpuPercentTotal = [math]::Round($cpuTotal, 2)
        WorkingSetMiB = [math]::Round($workingSetTotal, 1)
        PrivateMiB = [math]::Round($privateTotal, 1)
        GpuDedicatedMiB = $gpuDedicated
        GpuSharedMiB = $gpuShared
        GpuEnginePeakPercent = $gpuPeak
        GpuEngineCurrentPercent = $gpuCurrent
        VideoEncodePeakPercent = $videoEncodePeak
        VideoDecodePeakPercent = $videoDecodePeak
        VideoDecodeCurrentPercent = $videoDecodeCurrent
        VideoCodecPeakPercent = $videoCodecPeak
        VideoCodecCurrentPercent = $videoCodecCurrent
        GpuCounterInterpretation = $gpuCounterInterpretation
    }
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
Add-Line "Note: the Screen Mirror pairing PIN is redacted from this report."

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

Add-CommandOutput "Display Adapters" {
    Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue |
        Select-Object Name, PNPDeviceID, DriverVersion, AdapterRAM, VideoProcessor, Status |
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
    $script:sampledProcessResources = @($resourceRows)
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
            $gpuMemoryRow = [pscustomobject]@{
                Id = $process.Id
                GpuDedicatedMiB = [math]::Round($dedicated / 1MB, 1)
                GpuSharedMiB = [math]::Round($shared / 1MB, 1)
            }
            $script:sampledGpuMemory += $gpuMemoryRow
            $gpuMemoryRow | Format-Table -AutoSize

            $engines = Get-Counter -Counter `
                "\GPU Engine(pid_$($process.Id)*)\Utilization Percentage" `
                -SampleInterval 1 `
                -MaxSamples 5 `
                -ErrorAction Stop
            $script:gpuEngineSampleSucceeded = $true
            $engineRows = @($engines.CounterSamples |
                Group-Object InstanceName |
                ForEach-Object {
                    $samples = @($_.Group)
                    [pscustomobject]@{
                        Id = $process.Id
                        Engine = $_.Name
                        MaxPercent = [math]::Round((($samples | Measure-Object CookedValue -Maximum).Maximum), 2)
                        AveragePercent = [math]::Round((($samples | Measure-Object CookedValue -Average).Average), 2)
                        CurrentPercent = [math]::Round(($samples | Select-Object -Last 1).CookedValue, 2)
                    }
                } |
                Sort-Object MaxPercent -Descending)
            $script:sampledGpuEngines += $engineRows
            $activeEngineRows = @($engineRows | Where-Object MaxPercent -GT 0.01)
            if ($activeEngineRows.Count -eq 0) {
                "No active GPU engine was observed for PID $($process.Id) during the sample."
            } else {
                $activeEngineRows | Format-Table -AutoSize
            }
        } catch {
            "GPU counters unavailable for PID $($process.Id): $($_.Exception.Message)"
        }
    }
}

Add-CommandOutput "Screen Mirror Resource Summary" {
    Get-ScreenMirrorResourceSummary | Format-List
}

Add-CommandOutput "GPU Acceleration Verdict" {
    Get-GpuAccelerationVerdict | Format-List
}

Add-CommandOutput "Receiver Playback Route" {
    Get-ReceiverPlaybackRoute | Format-List
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

Add-CommandOutput "Sender Virtual Display Readiness" {
    Get-SenderVirtualDisplayVerdict | Format-List
}

Add-CommandOutput "Bundled Runtime" {
    $appRoot = Split-Path -Parent $screenMirror
    $requiredFiles = @(
        "glib-2.0-0.dll",
        "gobject-2.0-0.dll",
        "gstreamer-1.0-0.dll",
        "lib\gstreamer-1.0\gstd3d12.dll",
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
    Add-Line (Get-RedactedConfigText $configText)
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
    # Background checks only install while the app is idle, so a machine that streams around the
    # clock can sit on an old build. Show what the updater actually decided.
    $updaterLines = if (Test-Path -LiteralPath $logPath) {
        @(Get-Content -LiteralPath $logPath -Tail 2000 -ErrorAction SilentlyContinue |
            Where-Object { $_ -match ($script:logMessageAnchor + '(?:background|manual) update') })
    } else {
        @()
    }
    "---- updater log ----"
    if ($updaterLines.Count -eq 0) {
        "No update check has been recorded in the log tail."
    } else {
        $updaterLines | Select-Object -Last 10
    }

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
