param([string]$ProbePath = (Join-Path (Split-Path -Parent $PSScriptRoot) 'probes/win_probe.ps1'))
$ErrorActionPreference = 'Stop'
$bytes = [IO.File]::ReadAllBytes($ProbePath)
if ($bytes.Length -lt 4 -or $bytes[0] -ne 0xEF -or $bytes[1] -ne 0xBB -or $bytes[2] -ne 0xBF) { throw 'Probe BOM is missing' }
foreach ($b in $bytes[3..($bytes.Length - 1)]) { if ($b -gt 0x7F) { throw 'Probe is not ASCII' } }
$source = [Text.Encoding]::UTF8.GetString($bytes)
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Probe syntax is invalid' }
$names = @('R1', 'Get-KtMemoryNumber', 'Get-KtMemoryValues')
$funcs = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $names -contains $node.Name }, $true))
foreach ($name in $names) { if (@($funcs | Where-Object Name -eq $name).Count -ne 1) { throw "Memory function unavailable: $name" } }
$calls = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Get-KtMemoryValues' }, $true))
if ($calls.Count -ne 1) { throw 'Memory probe does not call its validated reader' }
foreach ($func in $funcs) { . ([scriptblock]::Create($func.Extent.Text)) }

function Check-Values($actual, $expected) {
  foreach ($key in @('total_gb', 'free_gb', 'available_pct', 'commit_pct')) {
    if ($actual[$key] -ne $expected[$key]) { throw "Memory mismatch: $key" }
  }
}
$gibKiB = 1048576
$normal = [pscustomobject]@{ TotalVisibleMemorySize = 16*$gibKiB; FreePhysicalMemory = 4*$gibKiB; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = 10*$gibKiB }
Check-Values (Get-KtMemoryValues $normal) @{ total_gb = 16; free_gb = 4; available_pct = 25; commit_pct = 50 }
$missing = [pscustomobject]@{ TotalVisibleMemorySize = 16*$gibKiB; FreePhysicalMemory = $null; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = $null }
Check-Values (Get-KtMemoryValues $missing) @{ total_gb = 16; free_gb = $null; available_pct = $null; commit_pct = $null }
$zero = [pscustomobject]@{ TotalVisibleMemorySize = 16*$gibKiB; FreePhysicalMemory = 0; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = 0 }
Check-Values (Get-KtMemoryValues $zero) @{ total_gb = 16; free_gb = 0; available_pct = 0; commit_pct = 100 }
$invalid = [pscustomobject]@{ TotalVisibleMemorySize = 16*$gibKiB; FreePhysicalMemory = -1; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = 21*$gibKiB }
Check-Values (Get-KtMemoryValues $invalid) @{ total_gb = 16; free_gb = $null; available_pct = $null; commit_pct = $null }
$partial = [pscustomobject]@{ TotalVisibleMemorySize = $null; FreePhysicalMemory = 4*$gibKiB; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = 0 }
Check-Values (Get-KtMemoryValues $partial) @{ total_gb = $null; free_gb = 4; available_pct = $null; commit_pct = 100 }
$over = [pscustomobject]@{ TotalVisibleMemorySize = 16*$gibKiB; FreePhysicalMemory = 17*$gibKiB; TotalVirtualMemorySize = 0; FreeVirtualMemory = 0 }
Check-Values (Get-KtMemoryValues $over) @{ total_gb = 16; free_gb = $null; available_pct = $null; commit_pct = $null }
$malformed = [pscustomobject]@{ TotalVisibleMemorySize = 'NaN'; FreePhysicalMemory = $true; TotalVirtualMemorySize = 20*$gibKiB; FreeVirtualMemory = 'not-a-number' }
Check-Values (Get-KtMemoryValues $malformed) @{ total_gb = $null; free_gb = $null; available_pct = $null; commit_pct = $null }
Check-Values (Get-KtMemoryValues $null) @{ total_gb = $null; free_gb = $null; available_pct = $null; commit_pct = $null }
Write-Output 'PASS: synthetic Windows memory observations'
