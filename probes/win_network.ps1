param([switch]$Probe)
# Read-only connectivity. No CIM, sockets, resolver names, payload or settings.
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$ErrorActionPreference = "Stop"
$env:PSModulePath = "$PSHOME\Modules;" + $env:PSModulePath
function Run-Bounded($Command, $CommandArgs) {
    $p = New-Object System.Diagnostics.Process
    try {
        $p.StartInfo.FileName = $Command
        $p.StartInfo.Arguments = (@($CommandArgs | ForEach-Object { if ($_ -match "\s") { [char]34 + $_ + [char]34 } else { $_ } }) -join " ")
        $p.StartInfo.UseShellExecute = $false
        $p.StartInfo.CreateNoWindow = $true
        $p.StartInfo.RedirectStandardOutput = $true
        $p.StartInfo.RedirectStandardError = $true
        $null = $p.Start()
        $o = $p.StandardOutput.ReadToEndAsync()
        $e = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit(5000)) { $p.Kill(); $null = $p.WaitForExit(1000); return @{status=28;out=""} }
        return @{status=$p.ExitCode;out=$o.GetAwaiter().GetResult()}
    } catch [System.ComponentModel.Win32Exception] {
        return @{status=if ($_.Exception.NativeErrorCode -eq 2) {-2} else {$null};out=""}
    } catch { return @{status=$null;out=""} }
    finally { $p.Dispose() }
}
function Active-Table($Result, $Family) {
    if ($Result.status -ne 0) { return "" }
    $sections = [regex]::Split($Result.out, "(?m)^={3,}[ \t]*\r?$")
    for ($i=0; $i -lt $sections.Count; $i++) {
        if ($sections[$i] -match ("(?m)^\s*IPv" + $Family + "\b") -and $i+2 -lt $sections.Count) { return $sections[$i+1] }
    }
    return ""
}
function Test-UsableAddress($Address) {
    try {
        if ($null -eq $Address) { return $false }
        if ($Address.IsIPv4MappedToIPv6) { $Address = $Address.MapToIPv4() }
        if ([Net.IPAddress]::IsLoopback($Address) -or $Address.Equals([Net.IPAddress]::Any) -or $Address.Equals([Net.IPAddress]::IPv6Any) -or $Address.IsIPv6LinkLocal -or $Address.IsIPv6Multicast) { return $false }
        $b = $Address.GetAddressBytes()
        if ($b.Length -eq 4 -and (($b[0] -eq 169 -and $b[1] -eq 254) -or $b[0] -ge 224)) { return $false }
        return $true
    } catch { return $false }
}
function Inspect-Network {
    $out = [ordered]@{schema="katala_network_check.v1";platform="win32";default_route_present=$null;ip_address_present=$null;dns_configured=$null;gateway_configured=$null;interfaces_up=$null}
    try {
        $a = @([Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() | Where-Object {$_.OperationalStatus -eq "Up" -and $_.NetworkInterfaceType -ne "Loopback"})
        $p = @($a | ForEach-Object {$_.GetIPProperties()})
        $ip = @($p | ForEach-Object {$_.UnicastAddresses} | Where-Object {Test-UsableAddress $_.Address})
        $g = @($p | ForEach-Object {$_.GatewayAddresses} | Where-Object {$_.Address.ToString() -notin @("0.0.0.0","::")})
        $d = @($p | ForEach-Object {$_.DnsAddresses})
        $out.interfaces_up=$a.Count; $out.ip_address_present=($ip.Count -gt 0); $out.gateway_configured=($g.Count -gt 0); $out.dns_configured=($d.Count -gt 0)
    } catch { }
    $v4 = Active-Table (Run-Bounded "$env:SystemRoot\System32\route.exe" @("print","-4")) 4
    $v6 = Active-Table (Run-Bounded "$env:SystemRoot\System32\route.exe" @("print","-6")) 6
    $out.default_route_present=Route-State $v4 $v6
    return $out
}
function Route-State($V4, $V6) {
    if ($V4 -match "(?m)^\s*0\.0\.0\.0\s+0\.0\.0\.0\s+\S+\s+\d+\.\d+\.\d+\.\d+\s+\d+\s*$" -or $V6 -match "(?m)^\s*\d+\s+\d+\s+::/0\s+\S+[ \t]*\r?$") { return $true }
    $known4=$V4 -match "(?m)^\s*\d+\.\d+\.\d+\.\d+\s+\d+\.\d+\.\d+\.\d+\s+\S+\s+\d+\.\d+\.\d+\.\d+\s+\d+\s*$"
    $known6=$V6 -match "(?m)^\s*\d+\s+\d+\s+\S+/\d+\s+\S+[ \t]*\r?$"
    if ($known4 -and $known6) { return $false }
    return $null
}
function Probe-Https($Url) {
    $r = Run-Bounded "$env:SystemRoot\System32\curl.exe" @("-q","--proto","=https","--tlsv1.2","--connect-timeout","2","--max-time","4","--silent","--output","NUL","--write-out","%{http_code} %{time_namelookup} %{time_connect} %{time_appconnect} %{time_starttransfer} %{time_total}",$Url)
    $f = @($r.out.Trim() -split "\s+")
    if ($r.status -eq 0 -and $f.Count -gt 0 -and $f[0] -match "^[1-5][0-9][0-9]$") {
        $item = @{state="reachable";http_status=[int]$f[0];timings=@{}}
        $keys = @("dns_ms","connect_ms","tls_ms","first_byte_ms","total_ms")
        for ($i=1; $i -lt [Math]::Min($f.Count,6); $i++) {
            $v=0.0
            if ([double]::TryParse($f[$i],[Globalization.NumberStyles]::Float,[Globalization.CultureInfo]::InvariantCulture,[ref]$v) -and $v -ge 0 -and $v -le 60) { $item.timings[$keys[$i-1]]=[Math]::Round($v*1000,3) }
        }
        return $item
    }
    $state=switch ($r.status) {-2 {"tool_missing"} 6 {"dns_failed"} 7 {"connect_failed"} 28 {"timed_out"} 35 {"tls_failed"} 60 {"tls_certificate_failed"} default {"unavailable"}}
    return @{state=$state}
}
if ($MyInvocation.InvocationName -ne ".") {
    $out=Inspect-Network; $out.active_probes=[bool]$Probe
    if ($Probe) { $out.https=@{named=(Probe-Https "https://www.apple.com/");fixed_ip=(Probe-Https "https://1.1.1.1/cdn-cgi/trace")} }
    $out | ConvertTo-Json -Depth 8 -Compress
}
