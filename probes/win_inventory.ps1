# katala-tune Windows inventory. Read-only: prints one JSON line to stdout.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Reads install locations directly (no winget / choco / scoop processes, no network):
#   winreg: Uninstall keys (HKLM 64/32-bit, HKCU). System components and updates are skipped.
#   scoop / choco / npm / cargo / uv / mise / bin (~/.local/bin): their install directories.
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$t0 = Get-Date
$items = New-Object System.Collections.ArrayList
$errors = New-Object System.Collections.ArrayList

function Add-Item($source, $name, $version, $explicit = $true, $publisher = $null) {
  $o = [ordered]@{ source = $source; name = [string]$name; version = $version; explicit = [bool]$explicit }
  if ($publisher) { $o.publisher = [string]$publisher }
  [void]$items.Add($o)
}
function Dirs($p) { if ($p -and (Test-Path -LiteralPath $p)) { @(Get-ChildItem -LiteralPath $p -Directory -Force | Where-Object { $_.Name -notlike '.*' }) } else { @() } }

try {
  $seen = @{}
  $keys = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*', 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
  foreach ($k in $keys) {
    foreach ($e in @(Get-ItemProperty -Path $k)) {
      $n = [string]$e.DisplayName
      if (-not $n -or $e.SystemComponent -eq 1 -or $e.ParentKeyName -or $e.ReleaseType -match 'Update|Hotfix') { continue }
      if ($seen.ContainsKey($n)) { continue }
      $seen[$n] = $true
      Add-Item 'winreg' $n ([string]$e.DisplayVersion) $true $e.Publisher
    }
  }
} catch { [void]$errors.Add("winreg: $_") }

try {
  foreach ($d in (Dirs (Join-Path $env:USERPROFILE 'scoop\apps'))) {
    if ($d.Name -eq 'scoop') { continue }
    $v = $null; $m = Join-Path $d.FullName 'current\manifest.json'
    if (Test-Path -LiteralPath $m) { $v = (Get-Content -LiteralPath $m -Raw | ConvertFrom-Json).version }
    Add-Item 'scoop' $d.Name $v
  }
} catch { [void]$errors.Add("scoop: $_") }

try {
  foreach ($d in (Dirs (Join-Path $env:ProgramData 'chocolatey\lib'))) {
    $v = $null; $ns = Get-ChildItem -LiteralPath $d.FullName -Filter *.nuspec | Select-Object -First 1
    if ($ns) { $v = ([xml](Get-Content -LiteralPath $ns.FullName -Raw)).package.metadata.version }
    Add-Item 'choco' $d.Name $v
  }
} catch { [void]$errors.Add("choco: $_") }

try {
  $root = Join-Path $env:APPDATA 'npm\node_modules'
  foreach ($d in (Dirs $root)) {
    $pkgs = if ($d.Name -like '@*') { @(Dirs $d.FullName | ForEach-Object { "$($d.Name)/$($_.Name)" }) } else { @($d.Name) }
    foreach ($p in $pkgs) {
      if ($p -eq 'npm' -or $p -eq 'corepack') { continue }
      $v = $null; $pj = Join-Path $root (Join-Path $p 'package.json')
      if (Test-Path -LiteralPath $pj) { $v = (Get-Content -LiteralPath $pj -Raw | ConvertFrom-Json).version }
      Add-Item 'npm' $p $v
    }
  }
} catch { [void]$errors.Add("npm: $_") }

try {
  $skip = 'cargo', 'rustc', 'rustup', 'rustdoc', 'rustfmt', 'cargo-fmt', 'cargo-clippy', 'clippy-driver', 'rust-analyzer', 'rust-gdb', 'rust-lldb'
  $cb = Join-Path $env:USERPROFILE '.cargo\bin'
  if (Test-Path -LiteralPath $cb) { foreach ($f in @(Get-ChildItem -LiteralPath $cb -Filter *.exe)) { if ($skip -notcontains $f.BaseName) { Add-Item 'cargo' $f.BaseName $null } } }
  foreach ($d in (Dirs (Join-Path $env:APPDATA 'uv\tools'))) { Add-Item 'uv' $d.Name $null }
  $lb = Join-Path $env:USERPROFILE '.local\bin'
  if (Test-Path -LiteralPath $lb) { foreach ($f in @(Get-ChildItem -LiteralPath $lb -Filter *.exe)) { Add-Item 'bin' $f.BaseName $null } }
  foreach ($d in (Dirs (Join-Path $env:LOCALAPPDATA 'mise\installs'))) {
    $v = @(Dirs $d.FullName | Where-Object { $_.Name -match '^\d' } | Sort-Object LastWriteTime -Descending | Select-Object -First 1)
    if ($v.Count) { Add-Item 'mise' $d.Name $v[0].Name }
  }
} catch { [void]$errors.Add("dev: $_") }

$result = [ordered]@{ os = 'windows'; items = @($items); errors = @($errors); elapsed_s = [math]::Round(((Get-Date) - $t0).TotalSeconds, 2) }
$result | ConvertTo-Json -Depth 4 -Compress
