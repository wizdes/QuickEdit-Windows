# Writes packaging\NOTICES.txt: the outside crates inside QuickEdit.exe, each with its licence and
# the licence text from its own folder. The list comes from Cargo.lock, by way of cargo metadata.
#
#   pwsh -NoProfile -File packaging\make-notices.ps1          writes the file
#   pwsh -NoProfile -File packaging\make-notices.ps1 -Check   fails if the file is out of date
#
# It walks the app's normal dependencies, so it lists code in the exe and the macros that made
# some of that code. Crates used only by tests and trial programs are left out.
[CmdletBinding()]
param([switch]$Check)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$repo = Split-Path -Parent $PSScriptRoot
$out = Join-Path $PSScriptRoot 'NOTICES.txt'
$json = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc `
    --manifest-path (Join-Path $repo 'Cargo.toml')
$meta = ($json -join "`n") | ConvertFrom-Json -Depth 64

$byId = @{}
foreach ($package in $meta.packages) { $byId[$package.id] = $package }
$nodes = @{}
foreach ($node in $meta.resolve.nodes) { $nodes[$node.id] = $node }

$app = $meta.packages | Where-Object { $_.name -eq 'qe-app' }
if (-not $app) { throw 'cargo metadata has no qe-app package.' }

# Walks only normal dependencies, since dev and build dependencies do not reach the exe.
$seen = @{}
$queue = [System.Collections.Generic.Queue[string]]::new()
$queue.Enqueue($app.id)
while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if ($seen.ContainsKey($id)) { continue }
    $seen[$id] = $true
    foreach ($dep in $nodes[$id].deps) {
        $normal = @($dep.dep_kinds | Where-Object { $null -eq $_.kind })
        if ($normal.Count -gt 0) { $queue.Enqueue($dep.pkg) }
    }
}

$outside = $seen.Keys | ForEach-Object { $byId[$_] } |
    Where-Object { $null -ne $_.source } |
    Sort-Object name, version
if (-not $outside) { throw 'No outside crates were found, so the walk above is wrong.' }

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('Third-party notices for QuickEdit')
$lines.Add('')
$lines.Add('packaging\make-notices.ps1 makes this file from Cargo.lock. It is a list made by a')
$lines.Add('script, and it is not legal advice. A person checks it before each release.')
$lines.Add('')
$lines.Add('QuickEdit itself is under the MIT licence. See LICENSE.')
$lines.Add('')
$lines.Add('Also inside the exe, and not listed by Cargo.lock:')
$lines.Add('- The Rust standard library: MIT OR Apache-2.0, https://github.com/rust-lang/rust')
$lines.Add('- The Microsoft C runtime, linked into the exe by the Visual Studio build tools, under')
$lines.Add('  the terms of those tools.')
$lines.Add('')
$lines.Add('Outside crates:')
foreach ($crate in $outside) {
    $license = if ($crate.license) { $crate.license } else { 'NO LICENSE FIELD: CHECK BY HAND' }
    $lines.Add("- $($crate.name) $($crate.version): $license")
}
foreach ($crate in $outside) {
    $folder = Split-Path -Parent $crate.manifest_path
    $files = @(Get-ChildItem $folder -File |
        Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE)' } |
        Sort-Object Name)
    $lines.Add('')
    $lines.Add(('=' * 72))
    $lines.Add("$($crate.name) $($crate.version)")
    if ($files.Count -eq 0) {
        $lines.Add('The crate holds no licence file. Its licence field says: ' + $crate.license)
    }
    foreach ($file in $files) {
        $lines.Add(('-' * 72))
        $lines.Add($file.Name)
        $lines.Add('')
        foreach ($line in (Get-Content $file.FullName)) { $lines.Add($line.TrimEnd()) }
    }
}
$text = ($lines -join "`n") + "`n"

if ($Check) {
    if (-not (Test-Path $out)) { throw "$out is missing. Run this script with no flag." }
    $now = [System.IO.File]::ReadAllText($out).Replace("`r`n", "`n")
    if ($now -ne $text) { throw "$out is out of date. Run this script with no flag." }
    Write-Host 'NOTICES.txt is current.'
    exit 0
}
[System.IO.File]::WriteAllText($out, $text, [System.Text.UTF8Encoding]::new($false))
Write-Host "Wrote $out with $(@($outside).Count) crates."
