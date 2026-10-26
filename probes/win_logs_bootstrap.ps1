$ErrorActionPreference="Stop"
[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false)
try {
$p=[Console]::In.ReadToEnd().Split([char]10)
if($p.Length -ne 8 -or $p[0] -cne "KTLOG1"){throw "frame"}
if($p[1] -cne "events" -and $p[1] -cne "logons"){throw "kind"}
$a=@()
foreach($v in $p[2..4]){
if($v -cnotmatch "^(0|-?[1-9][0-9]{0,18})$"){throw "cursor"}
$n=[long]::Parse($v,[Globalization.CultureInfo]::InvariantCulture)
if($p[1] -ceq "events"){
if($n -lt -9007199254740991 -or $n -gt 9007199254740991){throw "cursor"}
}else{if($n -lt 0){throw "cursor"}}
$a+= $n
}
if($p[1] -ceq "logons" -and $a[2] -ne 0){throw "cursor"}
if($p[5] -cnotmatch "^[1-9][0-9]{0,6}$" -or [int]$p[5] -gt 1048576 -or $p[6] -cnotmatch "^[0-9a-f]{64}$"){throw "length"}
$b=[Convert]::FromBase64String($p[7])
if($b.Length -ne [int]$p[5] -or [Convert]::ToBase64String($b) -cne $p[7]){throw "body"}
$sha=[Security.Cryptography.SHA256]::Create()
try{$h=[BitConverter]::ToString($sha.ComputeHash($b)).Replace("-","").ToLowerInvariant()}finally{$sha.Dispose()}
if($h -cne $p[6]){throw "hash"}
$utf8=New-Object System.Text.UTF8Encoding($false,$true)
$s=[ScriptBlock]::Create($utf8.GetString($b))
if($p[1] -ceq "events"){
& $s -SysCursor $a[0] -AppCursor $a[1] -NeonCursor $a[2]
$ok=$?
}else{& $s -SecCursor $a[0] -SecTime $a[1];$ok=$?}
if(-not $ok){throw "execution"}
exit 0
}catch{[Console]::Error.WriteLine("Windows log transport failed");exit 1}
