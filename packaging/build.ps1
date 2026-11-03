# Builds the package. packaging/README.md says when to use each form.
#
#   pwsh -NoProfile -File packaging\build.ps1               target\QuickEdit-<version>-x64.msix
#   pwsh -NoProfile -File packaging\build.ps1 -LooseLayout  target\package only, for
#                                                           Add-AppxPackage -Register
#
# The release profile in the root Cargo.toml sets fat LTO. The version comes from
# packaging\AppxManifest.xml and nowhere else. Any failure stops the script with a non-zero exit.
#
# This is the only build for the Store: it keeps this PC's folders out of the exe, and it checks
# the exe. .cargo/config.toml sets the C runtime rule, and Cargo joins the two lists of flags.
[CmdletBinding()]
param([switch]$LooseLayout)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$repo = Split-Path -Parent $PSScriptRoot
$target = Join-Path $repo 'target'
$layout = Join-Path $target 'package'
$packagedExe = $null

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

# Cargo finds .cargo\config.toml, with its C runtime rule, and the toolchain file from the current
# folder. So this must not depend on the folder the script was started from.
Push-Location $repo
try {

$manifestPath = Join-Path $PSScriptRoot 'AppxManifest.xml'
[xml]$manifest = Get-Content -Raw $manifestPath
$identity = $manifest.Package.Identity
$version = $identity.Version
$executable = $manifest.Package.Applications.Application.Executable
$makepri = Find-SdkTool 'makepri.exe'
$makeappx = Find-SdkTool 'makeappx.exe'

# These two would replace Cargo's list, and with it the rules this script relies on.
foreach ($name in 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS') {
    if (Test-Path "env:$name") { throw "$name is set. Clear it, because it would replace .cargo\config.toml." }
}

& pwsh -NoProfile -File (Join-Path $PSScriptRoot 'make-notices.ps1') -Check

# The last rule that matches wins, so the wide one goes first. Each path is a TOML literal string,
# which keeps its backslashes and cannot hold a single quote.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $HOME '.rustup' }
$sysroot = (& rustc --print sysroot).Trim()
$remaps = @(
    @($HOME, '/home'),
    @($cargoHome, '/cargo'),
    @($rustupHome, '/rustup'),
    @($sysroot, '/rust'),
    @($repo, '/src')
)
$flags = foreach ($remap in $remaps) {
    if ($remap[0].Contains("'")) { throw "This build cannot handle a quote in the path $($remap[0])." }
    "'--remap-path-prefix=$($remap[0])=$($remap[1])'"
}
$rustflags = 'target.x86_64-pc-windows-msvc.rustflags=[' + ($flags -join ',') + ']'

Write-Host "Building $executable $version in release..."
# --target-dir beats CARGO_TARGET_DIR and build.target-dir, which would leave an old exe here.
& cargo build --release --locked -p qe-app --bin QuickEdit --manifest-path (Join-Path $repo 'Cargo.toml') --target-dir $target --config $rustflags
$exe = Join-Path $target "release\$executable"
if (-not (Test-Path $exe)) { throw "The build gave no $exe." }

# The same two facts that crates\qe-app\tests\package.rs checks, for a build where no test runs.
# These read the whole file as text, which is stricter than the import table.
$bytes = [System.IO.File]::ReadAllBytes($exe)
$text = [System.Text.Encoding]::Latin1.GetString($bytes)
$runtime = [regex]::Match($text, '(?i)(VCRUNTIME\d+[_\d]*\.dll|MSVCP\d+[_\d]*\.dll|api-ms-win-crt-[a-z-]+)')
if ($runtime.Success) { throw "$exe still names $($runtime.Value), so a clean PC may not run it." }
$userFolder = [regex]::Match($text, '(?i)\\Users\\[^\x00]{0,60}')
if ($userFolder.Success) { throw "$exe holds a user folder: $($userFolder.Value)" }

if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
New-Item -ItemType Directory -Force $layout | Out-Null
Copy-Item $exe $layout
Copy-Item $manifestPath $layout
Copy-Item -Recurse (Join-Path $PSScriptRoot 'Assets') $layout
Copy-Item (Join-Path $PSScriptRoot 'NOTICES.txt') $layout
Copy-Item (Join-Path $repo 'LICENSE') $layout

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

# The tests that a normal run ignores, on the exact exe that goes in the package. The count
# must match, so a test that is renamed cannot drop out of this with no word.
$packagedExe = Join-Path $layout $executable
$env:QE_RELEASE_EXE = $packagedExe
try {
    $results = & cargo test --locked -p qe-app --test package --target-dir $target -- --ignored the_release_exe_ 2>&1 | Out-String
}
finally {
    Remove-Item env:QE_RELEASE_EXE
}
Write-Host $results
if ($results -notmatch 'test result: ok\. 5 passed') { throw 'The release tests did not all run and pass.' }

$exeSize = (Get-Item $exe).Length
if ($LooseLayout) {
    Write-Host "Loose layout ready. $executable is $exeSize bytes."
    Write-Host "Register it with: Add-AppxPackage -Register `"$(Join-Path $layout 'AppxManifest.xml')`""
    return
}

$package = Join-Path $target "QuickEdit-$version-x64.msix"
& $makeappx pack /o /d $layout /p $package
if ($identity.Name -like 'PLACEHOLDER*' -or $identity.Publisher -like '*PLACEHOLDER*') {
    Write-Warning 'The manifest still has its placeholders, so Partner Center will refuse this package. See packaging\README.md.'
}
Write-Host "$package is $((Get-Item $package).Length) bytes. $executable is $exeSize bytes."

}
finally {
    Pop-Location
}
