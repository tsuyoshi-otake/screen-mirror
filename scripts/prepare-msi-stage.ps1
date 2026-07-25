param(
    [string] $StageDir = "installer\stage",
    [string] $GStreamerRoot = ""
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..")
$stage = Join-Path $repo $StageDir

if (-not $GStreamerRoot) {
    if ($env:GSTREAMER_1_0_ROOT_MSVC_X86_64) {
        $GStreamerRoot = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
    } else {
        $entries = @(
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
        )
        $entry = Get-ItemProperty $entries -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -match 'GStreamer 1\.0 .*MSVC x86_64' } |
            Select-Object -First 1
        if ($entry) {
            $GStreamerRoot = $entry.InstallLocation
        }
    }
}

if (-not $GStreamerRoot -or -not (Test-Path -LiteralPath $GStreamerRoot)) {
    throw "GStreamer MSVC x86_64 runtime root was not found."
}

New-Item -ItemType Directory -Force -Path $stage | Out-Null

$runtimeFiles = @()

function Copy-IfChanged([string] $source, [string] $destination) {
    $sourceItem = Get-Item -LiteralPath $source
    $destinationItem = Get-Item -LiteralPath $destination -ErrorAction SilentlyContinue
    if ($destinationItem -and
        $destinationItem.Length -eq $sourceItem.Length -and
        $destinationItem.LastWriteTimeUtc -ge $sourceItem.LastWriteTimeUtc) {
        return
    }
    Copy-Item -LiteralPath $source -Destination $destination -Force
}

Copy-IfChanged (Join-Path $repo "target\release\screen-mirror.exe") (Join-Path $stage "screen-mirror.exe")
Copy-IfChanged (Join-Path $repo "README.md") (Join-Path $stage "README.md")
Copy-IfChanged (Join-Path $repo "assets\screen-mirror.ico") (Join-Path $stage "screen-mirror.ico")

function Copy-RuntimeFile([string] $source, [string] $relativeDestination) {
    $destination = Join-Path $stage $relativeDestination
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
    Copy-IfChanged $source $destination
    $script:runtimeFiles += $relativeDestination
}

Get-ChildItem -LiteralPath (Join-Path $GStreamerRoot "bin") -File |
    Where-Object { $_.Extension -ieq ".dll" } |
    ForEach-Object { Copy-RuntimeFile $_.FullName $_.Name }

$pluginAllowList = @(
    'gstautodetect.dll',
    'gstcoreelements.dll',
    'gstd3d11.dll',
    'gstlibav.dll',
    'gstmediafoundation.dll',
    'gstnvcodec.dll',
    'gstqsv.dll',
    'gstrtp.dll',
    'gstrtpmanager.dll',
    'gstudp.dll',
    'gstvideoconvertscale.dll',
    'gstvideoparsersbad.dll',
    'gstvideorate.dll',
    'gstx264.dll'
)

Get-ChildItem -LiteralPath (Join-Path $GStreamerRoot "lib\gstreamer-1.0") -File |
    Where-Object { $_.Name -in $pluginAllowList } |
    ForEach-Object { Copy-RuntimeFile $_.FullName (Join-Path "lib\gstreamer-1.0" $_.Name) }

$scannerDir = Join-Path $GStreamerRoot "libexec\gstreamer-1.0"
if (Test-Path -LiteralPath $scannerDir) {
    Get-ChildItem -LiteralPath $scannerDir -File |
        Where-Object { $_.Extension -ieq ".exe" -or $_.Extension -ieq ".dll" } |
        ForEach-Object { Copy-RuntimeFile $_.FullName (Join-Path "libexec\gstreamer-1.0" $_.Name) }
}

$wxs = New-Object System.Text.StringBuilder
[void] $wxs.AppendLine('<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">')
[void] $wxs.AppendLine('  <Fragment>')
[void] $wxs.AppendLine('    <DirectoryRef Id="INSTALLFOLDER">')
[void] $wxs.AppendLine('      <Directory Id="LibFolder" Name="lib">')
[void] $wxs.AppendLine('        <Directory Id="GStreamerPluginFolder" Name="gstreamer-1.0" />')
[void] $wxs.AppendLine('      </Directory>')
[void] $wxs.AppendLine('      <Directory Id="LibexecFolder" Name="libexec">')
[void] $wxs.AppendLine('        <Directory Id="GStreamerLibexecFolder" Name="gstreamer-1.0" />')
[void] $wxs.AppendLine('      </Directory>')
[void] $wxs.AppendLine('    </DirectoryRef>')
[void] $wxs.AppendLine('  </Fragment>')
[void] $wxs.AppendLine('  <Fragment>')
[void] $wxs.AppendLine('    <ComponentGroup Id="RuntimeComponents">')

$index = 0
foreach ($relative in ($runtimeFiles | Sort-Object)) {
    $index++
    $normalized = $relative -replace '\\', '/'
    $directoryId = if ($normalized.StartsWith('lib/gstreamer-1.0/')) {
        'GStreamerPluginFolder'
    } elseif ($normalized.StartsWith('libexec/gstreamer-1.0/')) {
        'GStreamerLibexecFolder'
    } else {
        'INSTALLFOLDER'
    }
    $componentId = "RuntimeComponent$index"
    $fileId = "RuntimeFile$index"
    $source = '$(var.StageDir)\' + ($relative -replace '/', '\')
    [void] $wxs.AppendLine("      <Component Id=`"$componentId`" Directory=`"$directoryId`" Bitness=`"always64`">")
    [void] $wxs.AppendLine("        <File Id=`"$fileId`" Source=`"$source`" KeyPath=`"yes`" />")
    [void] $wxs.AppendLine("      </Component>")
    [void] $wxs.AppendLine("      <ComponentRef Id=`"$componentId`" />")
}

[void] $wxs.AppendLine('    </ComponentGroup>')
[void] $wxs.AppendLine('  </Fragment>')
[void] $wxs.AppendLine('</Wix>')

$runtimeWxs = Join-Path $repo "installer\RuntimeFiles.wxs"
[IO.File]::WriteAllText($runtimeWxs, $wxs.ToString(), [Text.UTF8Encoding]::new($false))

$fileCount = ($runtimeFiles | Measure-Object).Count
$stageBytes = (Get-ChildItem -LiteralPath $stage -Recurse -File | Measure-Object Length -Sum).Sum
Write-Host "Staged $fileCount runtime files from $GStreamerRoot"
Write-Host ("Stage size: {0:N1} MB" -f ($stageBytes / 1MB))
