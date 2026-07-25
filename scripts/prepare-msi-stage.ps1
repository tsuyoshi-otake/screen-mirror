param(
    [string] $StageDir = "installer\stage",
    [string] $GStreamerRoot = "",
    [bool] $IncludeVdd = $true
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

Copy-RuntimeFile (Join-Path $repo "scripts\install-bundled-vdd.ps1") "install-bundled-vdd.ps1"

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
    'gstplayback.dll',
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

if ($IncludeVdd) {
    $vddVersion = "25.7.23"
    $vddUrl = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/$vddVersion/VirtualDisplayDriver-x86.Driver.Only.zip"
    $vddLicenseUrl = "https://raw.githubusercontent.com/VirtualDrivers/Virtual-Display-Driver/master/LICENSE"
    $cache = Join-Path $repo "installer\cache"
    $vddZip = Join-Path $cache "VirtualDisplayDriver-$vddVersion.Driver.Only.zip"
    $vddExtract = Join-Path $cache "VirtualDisplayDriver-$vddVersion"
    New-Item -ItemType Directory -Force -Path $cache | Out-Null

    if (-not (Test-Path -LiteralPath $vddZip)) {
        Write-Host "Downloading Virtual Display Driver $vddVersion..."
        Invoke-WebRequest -Uri $vddUrl -OutFile $vddZip
    }

    Remove-Item -LiteralPath $vddExtract -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive -Path $vddZip -DestinationPath $vddExtract -Force
    $vddSource = Join-Path $vddExtract "VirtualDisplayDriver"
    $vddStage = Join-Path $stage "vdd"
    Remove-Item -LiteralPath $vddStage -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $vddStage | Out-Null
    Get-ChildItem -LiteralPath $vddSource -File |
        ForEach-Object { Copy-RuntimeFile $_.FullName (Join-Path "vdd" $_.Name) }

    $licenseStage = Join-Path $stage "licenses"
    New-Item -ItemType Directory -Force -Path $licenseStage | Out-Null
    $licenseFile = Join-Path $licenseStage "Virtual-Display-Driver-LICENSE.txt"
    Invoke-WebRequest -Uri $vddLicenseUrl -OutFile $licenseFile
    $script:runtimeFiles += "licenses\Virtual-Display-Driver-LICENSE.txt"
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
[void] $wxs.AppendLine('      <Directory Id="VddFolder" Name="vdd" />')
[void] $wxs.AppendLine('      <Directory Id="LicenseFolder" Name="licenses" />')
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
    } elseif ($normalized.StartsWith('vdd/')) {
        'VddFolder'
    } elseif ($normalized.StartsWith('licenses/')) {
        'LicenseFolder'
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
