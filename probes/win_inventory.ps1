# katala-tune Windows inventory. Read-only: prints one JSON line to stdout.
# PowerShell 5.1. Keep this file ASCII only (PS 5.1 reads BOM-less UTF-8 as the ANSI code page).
# Reads install locations directly (no winget / choco / scoop processes, no network):
#   winreg: Uninstall keys (HKLM 64/32-bit, HKCU). System components and updates are skipped.
#   scoop / choco / npm / cargo / uv / mise / bin (~/.local/bin): their install directories.
#   platform: package managers and runtimes themselves, and Store-delivered tools without an Uninstall entry.
# Errors are terminating ('Stop') so a failed read is reported, not mistaken for "nothing installed".
# A section that fails is listed in failed_sources; the caller then keeps the previous list for those sources.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$t0 = Get-Date
$items = New-Object System.Collections.ArrayList
$errors = New-Object System.Collections.ArrayList
$failed = New-Object System.Collections.ArrayList

function Add-Item($list, $source, $name, $version, $explicit = $true, $publisher = $null) {
  $o = [ordered]@{ source = $source; name = [string]$name; version = $version; explicit = [bool]$explicit }
  if ($publisher) { $o.publisher = [string]$publisher }
  [void]$list.Add($o)
}
# Missing directory = nothing installed. Unreadable directory = error (thrown).
function Dirs($p) { if ($p -and (Test-Path -LiteralPath $p)) { @(Get-ChildItem -LiteralPath $p -Directory -Force | Where-Object { $_.Name -notlike '.*' }) } else { @() } }
function Read-Json($p) { Get-Content -LiteralPath $p -Raw | ConvertFrom-Json }

# Run one section into a temporary list; keep its items only when the whole section succeeded.
function Section($sources, [scriptblock]$body) {
  $part = New-Object System.Collections.ArrayList
  try {
    & $body $part
    foreach ($o in $part) { [void]$items.Add($o) }
  } catch {
    [void]$errors.Add("$($sources -join '/'): $($_.Exception.Message)")
    foreach ($s in $sources) { [void]$failed.Add($s) }
  }
}

Section @('winreg') {
  param($out)
  $seen = @{}
  $unreadable = 0
  $roots = 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall', 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
  foreach ($root in $roots) {
    if (-not (Test-Path -LiteralPath $root)) { continue }
    foreach ($k in @(Get-ChildItem -LiteralPath $root)) {
      # A single unreadable entry (access denied) is skipped and counted; it never appears, so it never looks removed.
      try { $e = Get-ItemProperty -LiteralPath $k.PSPath } catch { $unreadable++; continue }
      $n = [string]$e.DisplayName
      if (-not $n -or $e.SystemComponent -eq 1 -or $e.ParentKeyName -or $e.ReleaseType -match 'Update|Hotfix') { continue }
      if ($seen.ContainsKey($n)) { continue }
      $seen[$n] = $true
      Add-Item $out 'winreg' $n ([string]$e.DisplayVersion) $true $e.Publisher
    }
  }
  if ($unreadable) { [void]$errors.Add("winreg: $unreadable entries unreadable (skipped)") }
}

Section @('scoop') {
  param($out)
  foreach ($d in (Dirs (Join-Path $env:USERPROFILE 'scoop\apps'))) {
    if ($d.Name -eq 'scoop') { continue }
    $v = $null; $m = Join-Path $d.FullName 'current\manifest.json'
    if (Test-Path -LiteralPath $m) { $v = (Read-Json $m).version }
    Add-Item $out 'scoop' $d.Name $v
  }
}

Section @('choco') {
  param($out)
  foreach ($d in (Dirs (Join-Path $env:ProgramData 'chocolatey\lib'))) {
    $v = $null; $ns = Get-ChildItem -LiteralPath $d.FullName -Filter *.nuspec | Select-Object -First 1
    if ($ns) { $v = ([xml](Get-Content -LiteralPath $ns.FullName -Raw)).package.metadata.version }
    Add-Item $out 'choco' $d.Name $v
  }
}

Section @('npm') {
  param($out)
  $root = Join-Path $env:APPDATA 'npm\node_modules'
  foreach ($d in (Dirs $root)) {
    $pkgs = if ($d.Name -like '@*') { @(Dirs $d.FullName | ForEach-Object { "$($d.Name)/$($_.Name)" }) } else { @($d.Name) }
    foreach ($p in $pkgs) {
      if ($p -eq 'npm' -or $p -eq 'corepack') { continue }
      $v = $null; $pj = Join-Path $root (Join-Path $p 'package.json')
      if (Test-Path -LiteralPath $pj) { $v = (Read-Json $pj).version }
      Add-Item $out 'npm' $p $v
    }
  }
}

Section @('cargo') {
  param($out)
  $skip = 'cargo', 'rustc', 'rustup', 'rustdoc', 'rustfmt', 'cargo-fmt', 'cargo-clippy', 'clippy-driver', 'rust-analyzer', 'rust-gdb', 'rust-lldb'
  $cb = Join-Path $env:USERPROFILE '.cargo\bin'
  if (Test-Path -LiteralPath $cb) { foreach ($f in @(Get-ChildItem -LiteralPath $cb -Filter *.exe)) { if ($skip -notcontains $f.BaseName) { Add-Item $out 'cargo' $f.BaseName $null } } }
}

Section @('uv') {
  param($out)
  foreach ($d in (Dirs (Join-Path $env:APPDATA 'uv\tools'))) { Add-Item $out 'uv' $d.Name $null }
}

Section @('bin') {
  param($out)
  $lb = Join-Path $env:USERPROFILE '.local\bin'
  if (Test-Path -LiteralPath $lb) { foreach ($f in @(Get-ChildItem -LiteralPath $lb -Filter *.exe)) { Add-Item $out 'bin' $f.BaseName $null } }
}

Section @('mise') {
  param($out)
  foreach ($d in (Dirs (Join-Path $env:LOCALAPPDATA 'mise\installs'))) {
    $v = @(Dirs $d.FullName | Where-Object { $_.Name -match '^\d' } | Sort-Object LastWriteTime -Descending | Select-Object -First 1)
    if ($v.Count) { Add-Item $out 'mise' $d.Name $v[0].Name }
  }
}

Section @('platform') {
  param($out)
  $wa = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps'
  $checks = [ordered]@{
    'winget' = @((Join-Path $wa 'winget.exe'))
    'windows-terminal' = @((Join-Path $wa 'wt.exe'))
    'wsl' = @((Join-Path $env:SystemRoot 'System32\wsl.exe'))
    'scoop' = @((Join-Path $env:USERPROFILE 'scoop\shims\scoop.ps1'))
    'chocolatey' = @((Join-Path $env:ProgramData 'chocolatey\bin\choco.exe'))
    'rustup' = @((Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'))
    'cargo' = @((Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'))
    'nvm' = @((Join-Path $env:APPDATA 'nvm\nvm.exe'), (Join-Path $env:LOCALAPPDATA 'nvm\nvm.exe'))
  }
  foreach ($k in $checks.Keys) { if (@($checks[$k] | Where-Object { Test-Path -LiteralPath $_ }).Count) { Add-Item $out 'platform' $k $null } }
}

$result = [ordered]@{ os = 'windows'; items = @($items); errors = @($errors); failed_sources = @($failed); elapsed_s = [math]::Round(((Get-Date) - $t0).TotalSeconds, 2) }
$result | ConvertTo-Json -Depth 4 -Compress
