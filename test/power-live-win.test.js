'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const {spawnSync} = require('node:child_process');

test('PS5.1 dmon values expire and process loss/restart never reuses GPU power', {skip:process.platform !== 'win32'}, () => {
  const source=fs.readFileSync('probes/live_win.ps1','utf8');
  const helpers=source.slice(source.indexOf('function StartDmon'),source.indexOf('# ---- Top processes'));
  const jsonHelpers=source.slice(source.indexOf('function N('),source.indexOf('function OpenCat'));
  const script=`$ErrorActionPreference="Stop"
$inv=[Globalization.CultureInfo]::InvariantCulture
$Interval=1;$smi="missing-sensor-executable";$gpuMaxAgeMs=3000
$gpuName=@{0="test GPU"};$gpuTotal=@{0=100};$gpuNow=@{}
$gpuSampleClock=[pscustomobject]@{Elapsed=[timespan]::FromMilliseconds(1000)}
${jsonHelpers}
${helpers}
function MockProcess($lines) {
 $q=New-Object "Collections.Generic.Queue[string]"
 foreach($line in $lines){$q.Enqueue($line)}
 $reader=[pscustomobject]@{Lines=$q}
 $reader|Add-Member ScriptMethod ReadLineAsync { $t=New-Object "Threading.Tasks.TaskCompletionSource[string]";if($this.Lines.Count){$t.SetResult($this.Lines.Dequeue())};return $t.Task }
 $p=[pscustomobject]@{HasExited=$false;StandardOutput=$reader;Disposed=$false}
 $p|Add-Member ScriptMethod Dispose {$this.Disposed=$true}
 return $p
}
$dmon=MockProcess @("# gpu pwr sm fb gclk mclk","0 25 4 50 500 800")
$dmonTask=$dmon.StandardOutput.ReadLineAsync();$dmonCols=$null
PollDmon
$fresh=GpuJson
$gpuSampleClock.Elapsed=[timespan]::FromMilliseconds(4000);$boundary=GpuJson
$gpuSampleClock.Elapsed=[timespan]::FromMilliseconds(4001);$expired=GpuJson
$gpuSampleClock.Elapsed=[timespan]::FromMilliseconds(1000)
$old=$dmon;$dmon.HasExited=$true;PollDmon;$lost=GpuJson
$disposed=$old.Disposed;$cleared=$null -eq $dmon -and $null -eq $dmonCols
$gpuNow[0]=@{power=25;received_ms=1000};$dmonCols=@("old");PollDmon
$restart=GpuJson;$reset=$null -eq $dmonCols
$dmon=MockProcess @();$dmonTask=[pscustomobject]@{IsCompleted=$true;Result=$null}
$gpuNow[0]=@{power=25;received_ms=1000};PollDmon;$eof=GpuJson
[ordered]@{fresh=$fresh;boundary=$boundary;expired=$expired;lost=$lost;disposed=$disposed;cleared=$cleared;restart=$restart;reset=$reset;eof=$eof}|ConvertTo-Json -Compress
`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],{encoding:'utf8',timeout:15000,windowsHide:true});
  assert.equal(r.status,0,r.stderr);
  const d=JSON.parse(r.stdout.trim());
  assert.equal(JSON.parse(d.fresh)[0].power_w,25);
  assert.equal(JSON.parse(d.boundary)[0].power_w,25);
  for(const k of ['expired','lost','restart','eof'])assert.deepEqual(JSON.parse(d[k]),[],k);
  for(const k of ['disposed','cleared','reset'])assert.equal(d[k],true,k);
});
