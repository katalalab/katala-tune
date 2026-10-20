[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$ErrorActionPreference = "Stop"
$worker = {
# KT_CAPABILITIES_WORKER_TEST_PREFIX
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

}
# Only the Process object created here may be terminated. No PID lookup or shared job.
$child = $null
$owned = $false
$exited = $false
$pipes = $false
$timedOut = $false
$report = $null
$outTask = $null
$errTask = $null
$watch = [Diagnostics.Stopwatch]::StartNew()
try {
    $bootstrap = "& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadLine()))))"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($bootstrap))
    $wire = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($worker.ToString())) + "`n"
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
    $psi.Arguments = "-NoLogo -NoProfile -NonInteractive -EncodedCommand " + $encoded
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $child = New-Object Diagnostics.Process
    $child.StartInfo = $psi
    if (-not $child.Start()) { throw "worker_start_failed" }
    $owned = $true
    # Force a retained native handle before cleanup; ID/start remain private evidence.
    $ownedHandle = $child.Handle
    $ownedId = $child.Id
    $ownedStart = $child.StartTime
    $outTask = $child.StandardOutput.ReadToEndAsync()
    $errTask = $child.StandardError.ReadToEndAsync()
    $writeTask = $child.StandardInput.WriteAsync($wire)
    $writeDeadline = [Math]::Min(2000, 17000 - [int]$watch.ElapsedMilliseconds)
    $writeStarted = [int]$watch.ElapsedMilliseconds
    if (-not $writeTask.Wait([Math]::Max(0, $writeDeadline))) { $timedOut = $true; throw "worker_stdin_timeout" }
    $flushTask = $child.StandardInput.FlushAsync()
    $flushLeft = [Math]::Max(0, $writeDeadline - ([int]$watch.ElapsedMilliseconds - $writeStarted))
    if (-not $flushTask.Wait($flushLeft)) { $timedOut = $true; throw "worker_stdin_timeout" }
    $child.StandardInput.Close()
    $runLeft = [Math]::Max(0, 17000 - [int]$watch.ElapsedMilliseconds)
    $exited = $child.WaitForExit($runLeft)
    if (-not $exited) { $timedOut = $true }
} catch {
    # Raw provider/process errors are intentionally discarded.
} finally {
    if ($owned -and -not $exited) {
        try {
            if (-not $child.HasExited) { $child.Kill() }
            $killLeft = [Math]::Max(0, 19500 - [int]$watch.ElapsedMilliseconds)
            $exited = $child.WaitForExit($killLeft)
        } catch { $exited = $false }
    }
    if ($exited -and $outTask -and $errTask) {
        try {
            $pipeDeadline = [Math]::Min(20000, [int]$watch.ElapsedMilliseconds + 500)
            $outDone = $outTask.Wait([Math]::Max(0, $pipeDeadline - [int]$watch.ElapsedMilliseconds))
            $errDone = $errTask.Wait([Math]::Max(0, $pipeDeadline - [int]$watch.ElapsedMilliseconds))
            $pipes = $outDone -and $errDone -and $outTask.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion -and $errTask.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion
        } catch { $pipes = $false }
    }
    $lifecycle = "cleanup_unconfirmed"
    if ($exited -and $pipes) {
        if ($timedOut) { $lifecycle = "timeout_cleanup_confirmed" }
        else {
            $lifecycle = "worker_failed"
            try {
                if ($child.ExitCode -eq 0) {
                    # Result is read only after the finite completion checks above.
                    $candidate = $outTask.Result | ConvertFrom-Json -ErrorAction Stop
                    if ($candidate.schema -eq "katala_capabilities.v1" -and $candidate.platform -eq "win32") {
                        $report = $candidate
                        $lifecycle = "completed"
                    }
                }
            } catch { $report = $null }
        }
    }
    if ($lifecycle -ne "completed") { $report = $null }
    if ($child) { $child.Dispose() }
}
@{ schema = "katala_capabilities_envelope.v1"; platform = "win32"; lifecycle = $lifecycle; worker_exit_confirmed = [bool]$exited; pipes_completed = [bool]$pipes; report = $report } | ConvertTo-Json -Depth 6 -Compress
