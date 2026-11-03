# Builds the package. packaging/README.md says when to use each form.
#
#   pwsh -NoProfile -File packaging\build.ps1               target\QuickEdit-<version>-x64.msix
#   pwsh -NoProfile -File packaging\build.ps1 -LooseLayout  target\package only, for
#                                                           Add-AppxPackage -Register
#
# The release profile in the root Cargo.toml sets fat LTO. The version comes from
# packaging\AppxManifest.xml and nowhere else. Any failure stops the script with a non-zero exit.
[CmdletBinding()]
param([switch]$LooseLayout)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$repo = Split-Path -Parent $PSScriptRoot
$target = Join-Path $repo 'target'
$layout = Join-Path $target 'package'

function Find-SdkTool([string]$name) {
    $bin = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $found = Get-ChildItem $bin -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\$name" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    if (-not $found) { throw "$name was not found under $bin. Install the Windows SDK." }
    return $found
}

$manifestPath = Join-Path $PSScriptRoot 'AppxManifest.xml'
[xml]$manifest = Get-Content -Raw $manifestPath
$identity = $manifest.Package.Identity
$version = $identity.Version
$executable = $manifest.Package.Applications.Application.Executable
$makepri = Find-SdkTool 'makepri.exe'
$makeappx = Find-SdkTool 'makeappx.exe'

Write-Host "Building $executable $version in release..."
& cargo build --release --locked -p qe-app --bin QuickEdit --manifest-path (Join-Path $repo 'Cargo.toml')
$exe = Join-Path $target "release\$executable"
if (-not (Test-Path $exe)) { throw "The build gave no $exe." }

if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
New-Item -ItemType Directory -Force $layout | Out-Null
Copy-Item $exe $layout
Copy-Item $manifestPath $layout
Copy-Item -Recurse (Join-Path $PSScriptRoot 'Assets') $layout

# resources.pri is the index that lets Windows find Square44x44Logo.scale-200.png and the rest
# when the manifest says only Square44x44Logo.png. Its config stays out of the package. The
# config's <packaging> part would split the index into one file per scale, which is for a bundle
# of resource packages. This is one flat package, so one index must name every file.
$config = Join-Path $target 'priconfig.xml'
& $makepri createconfig /cf $config /dq en-US /pv 10.0.0 /o
$configText = Get-Content -Raw $config
$flatConfig = $configText -replace '(?s)<packaging>.*?</packaging>', ''
if ($flatConfig -eq $configText) { throw "makepri's config has no <packaging> part to remove: $config" }
[System.IO.File]::WriteAllText($config, $flatConfig)
& $makepri new /pr $layout /cf $config /mn (Join-Path $layout 'AppxManifest.xml') /of (Join-Path $layout 'resources.pri') /o
Remove-Item $config

$exeSize = (Get-Item $exe).Length
if ($LooseLayout) {
    Write-Host "Loose layout ready. $executable is $exeSize bytes."
    Write-Host "Register it with: Add-AppxPackage -Register `"$(Join-Path $layout 'AppxManifest.xml')`""
    exit 0
}

$package = Join-Path $target "QuickEdit-$version-x64.msix"
& $makeappx pack /o /d $layout /p $package
if ($identity.Name -like 'PLACEHOLDER*' -or $identity.Publisher -like '*PLACEHOLDER*') {
    Write-Warning 'The manifest still has its placeholders, so Partner Center will refuse this package. See packaging\README.md.'
}
Write-Host "$package is $((Get-Item $package).Length) bytes. $executable is $exeSize bytes."
