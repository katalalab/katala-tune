[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$ErrorActionPreference = "Stop"
function ErrorState($failure) {
    $id = $failure.FullyQualifiedErrorId
    $exception = $failure.Exception
    # CIM access denial can be CimException NativeErrorCode 5 / HRESULT 0x80041003.
    if ($exception -is [System.UnauthorizedAccessException] -or $exception.HResult -eq -2147024891 -or $exception.HResult -eq -2147023582 -or $exception.HResult -eq -2147217405 -or $exception.NativeErrorCode -eq 5 -or [string]$exception.NativeErrorCode -eq "AccessDenied" -or ($exception -is [System.ComponentModel.Win32Exception] -and $exception.NativeErrorCode -in @(5,1314)) -or $id -match "UnauthorizedAccess|AccessDenied|PrivilegeNotHeld|SetPrivilegeFailed") {
        return "permission_denied"
    }
    if ($id -match "CommandNotFound") { return "tool_missing" }
    if ($exception -is [System.PlatformNotSupportedException] -or $id -match "PlatformNotSupported|CmdletNotSupported") {
        return "unsupported"
    }
    return "error"
}
$r = @{ schema = "katala_capabilities.v1"; platform = "win32"; inventory_status = "ok"; secure_boot = @{state = "error"} }
$failures = @()
try {
    $cs = Get-CimInstance -ClassName Win32_ComputerSystem -Property Manufacturer,Model -OperationTimeoutSec 5
    $r.manufacturer = $cs.Manufacturer
    $r.model = $cs.Model
} catch { $failures += (ErrorState $_) }
try {
    $board = Get-CimInstance -ClassName Win32_BaseBoard -Property Manufacturer,Product -OperationTimeoutSec 5
    $r.baseboard_manufacturer = $board.Manufacturer
    $r.baseboard_product = $board.Product
} catch { $failures += (ErrorState $_) }
try {
    $bios = Get-CimInstance -ClassName Win32_BIOS -Property SMBIOSBIOSVersion,ReleaseDate -OperationTimeoutSec 5
    $r.bios_version = $bios.SMBIOSBIOSVersion
    if ($bios.ReleaseDate) { $r.bios_date = $bios.ReleaseDate.ToString("yyyy-MM-dd") }
} catch { $failures += (ErrorState $_) }
if ($failures.Count -gt 0) {
    $r.inventory_status = "error"
    $r.inventory_reason = "error"
    if ($failures -contains "permission_denied") { $r.inventory_reason = "permission_denied" }
    elseif ($failures -contains "unsupported") { $r.inventory_reason = "unsupported" }
}
try {
    $enabled = Confirm-SecureBootUEFI
    if ($enabled -eq $true) { $r.secure_boot.state = "enabled" }
    elseif ($enabled -eq $false) { $r.secure_boot.state = "disabled" }
} catch { $r.secure_boot.state = ErrorState $_ }
$r | ConvertTo-Json -Depth 4 -Compress
