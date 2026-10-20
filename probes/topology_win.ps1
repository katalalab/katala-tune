# Read-only topology probe. TopologyRequest is injected by the stdin transport.
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$script:TopologyClock = [Diagnostics.Stopwatch]::StartNew()
$script:TopologyLimits = $TopologyRequest.limits
$TopologyWorkMilliseconds = if ($TopologyRequest.work_timeout_ms) {[double]$TopologyRequest.work_timeout_ms} else {[Math]::Max(100,[int]($TopologyLimits.remote_timeout_seconds*800))}
$script:TopologyWorkSeconds = [Math]::Max(0.1, $TopologyWorkMilliseconds / 1000)
$script:TopologyFailures = New-Object System.Collections.ArrayList
$script:TopologyRepositories = New-Object System.Collections.ArrayList
$script:TopologyCounters = [ordered]@{entries=0;files=0;bytes=0;repositories=0;manifests=0;children=0;duration_ms=0;config_files=0;config_keys=0}
$script:TopologyStop = $false
$script:TopologyStalled = $false
$script:TopologyRootIndex = 0
$script:TopologyFrames = New-Object System.Collections.ArrayList
$script:TopologyCache = @{}
$script:TopologyDirectoryCache = @{}
$script:TopologyActive = New-Object "System.Collections.Generic.HashSet[string]" ([StringComparer]::Ordinal)
$script:TopologySkip = @(".git",".branch-recovery","target","node_modules",".cache","cache","__pycache__",".venv","venv","vendor","dist","build")
$script:TopologyGit = $TopologyRequest.git_program
if (-not $TopologyGit) { $script:TopologyGit = "C:\Program Files\Git\cmd\git.exe" }
function Add-TopologyFailure([string]$Code,[string]$Message,$Repo=$null) {
    $TopologyCollection = $TopologyFailures
    if ($null -ne $Repo) { $TopologyCollection = $Repo.failures }
    if ($TopologyCollection.Count -lt 64 -and -not ($TopologyCollection | Where-Object {$_.code -eq $Code -and $_.message -eq $Message})) {[void]$TopologyCollection.Add(@{code=$Code;message=$Message})}
}
function Get-TopologyHash([string]$Value) {
    $TopologyHasher=[Security.Cryptography.SHA256]::Create()
    try {return ([BitConverter]::ToString($TopologyHasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)))).Replace("-","").ToLowerInvariant()} finally {$TopologyHasher.Dispose()}
}
function Get-TopologyCanonical($Value) {
    if ($null -eq $Value) {return "z"}
    if ($Value -is [bool]) {if ($Value) {return "b1"};return "b0"}
    if ($Value -is [string]) {$TopologyUtf8=New-Object Text.UTF8Encoding($false,$true);return "s"+$TopologyUtf8.GetByteCount($Value)+":"+$Value}
    if ($Value -is [ValueType]) {
        if ($Value -isnot [double] -and $Value -isnot [single] -and [Math]::Abs([decimal]$Value) -gt [decimal]9007199254740992) {throw "numeric range"}
        $TopologyNumber=[double]$Value
        if ([double]::IsNaN($TopologyNumber) -or [double]::IsInfinity($TopologyNumber) -or ([Math]::Truncate($TopologyNumber) -eq $TopologyNumber -and [Math]::Abs($TopologyNumber) -gt 9007199254740992)) {throw "numeric range"}
        if ($TopologyNumber -eq 0) {$TopologyNumber=[double]0}
        $TopologyNumberBytes=[BitConverter]::GetBytes($TopologyNumber)
        if ([BitConverter]::IsLittleEndian) {[Array]::Reverse($TopologyNumberBytes)}
        return "n"+([BitConverter]::ToString($TopologyNumberBytes)).Replace("-","").ToLowerInvariant()
    }
    if ($Value -is [array] -or $Value -is [System.Collections.IList]) {
        $TopologyParts=New-Object Text.StringBuilder
        foreach ($TopologyChild in $Value) {[void]$TopologyParts.Append((Get-TopologyCanonical $TopologyChild))}
        return "a"+$Value.Count+":["+$TopologyParts.ToString()+"]"
    }
    $TopologyKeys=New-Object "System.Collections.Generic.SortedDictionary[string,string]" ([StringComparer]::Ordinal)
    $TopologyMap=@{}
    $TopologyUtf8=New-Object Text.UTF8Encoding($false,$true)
    if ($Value -is [System.Collections.IDictionary]) {
        foreach ($TopologyKey in $Value.Keys) {$TopologyName=[string]$TopologyKey;$TopologySort=([BitConverter]::ToString($TopologyUtf8.GetBytes($TopologyName))).Replace("-","");$TopologyKeys.Add($TopologySort,$TopologyName);$TopologyMap[$TopologySort]=$Value[$TopologyKey]}
    } else {
        foreach ($TopologyProperty in $Value.PSObject.Properties) {$TopologyName=$TopologyProperty.Name;$TopologySort=([BitConverter]::ToString($TopologyUtf8.GetBytes($TopologyName))).Replace("-","");$TopologyKeys.Add($TopologySort,$TopologyName);$TopologyMap[$TopologySort]=$TopologyProperty.Value}
    }
    $TopologyParts=New-Object Text.StringBuilder
    foreach ($TopologyPair in $TopologyKeys.GetEnumerator()) {[void]$TopologyParts.Append((Get-TopologyCanonical $TopologyPair.Value));[void]$TopologyParts.Append((Get-TopologyCanonical $TopologyMap[$TopologyPair.Key]))}
    return "o"+$TopologyKeys.Count+":{"+$TopologyParts.ToString()+"}"
}
function Test-TopologyTime {
    if ($TopologyClock.Elapsed.TotalSeconds -ge $TopologyWorkSeconds) {Add-TopologyFailure "timeout" "probe time budget exceeded";return $false};return $true
}
function Test-TopologyRelative([string]$Value,[bool]$Entry=$false) {
    if ($Value -match "[\p{Cc}\\:]" -or $Value.StartsWith("/")) {return $false}
    if ($Entry) {return ($Value -eq "" -or ($Value -ne "." -and $Value -ne ".." -and -not $Value.Contains("/")))}
    if ($Value -eq ".") {return $true}
    return ($Value -ne "" -and -not @($Value -split "/" | Where-Object {$_ -eq "" -or $_ -eq "." -or $_ -eq ".."}).Count)
}
# Source permission is per request, never inferred from an earlier capture.
$script:TopologySourceDenied=$false
$script:TopologySources=New-Object "System.Collections.Generic.Dictionary[string,object]" ([StringComparer]::Ordinal)
function Test-TopologySourcePolicy {
    if ($null -eq $TopologyRequest.source_allowlist -or $TopologyRequest.source_allowlist -isnot [array] -or $TopologyRequest.source_allowlist.Count -eq 0 -or @($TopologyRequest.configs).Count -gt 0) {return $false}
    try {
        $TopologyTotalFiles=0;$TopologyTotalBytes=[long]0
        if ($TopologyRequest.source_allowlist.Count -gt $TopologyLimits.entries_per_environment) {return $false}
        foreach ($TopologyRawRoot in $TopologyRequest.roots) {
            $TopologyCanonical=$TopologyRawRoot.Replace("\","/")
            if ($TopologyCanonical -notmatch "^[a-zA-Z]:/" -or ($TopologyCanonical.Length -gt 3 -and @($TopologyCanonical.Substring(3).Split("/") | Where-Object {$_ -eq "" -or $_ -eq "." -or $_ -eq ".."}).Count -gt 0)) {return $false}
        }
        foreach ($TopologyAllowed in $TopologyRequest.source_allowlist) {
            if (@($TopologyAllowed.PSObject.Properties).Count -ne 4 -or $TopologyAllowed.root_index -isnot [int] -and $TopologyAllowed.root_index -isnot [long]) {return $false}
            if ($TopologyAllowed.root_index -lt 0 -or $TopologyAllowed.root_index -ge $TopologyRootPaths.Count -or -not (Test-TopologyRelative $TopologyAllowed.relative_path) -or $TopologyAllowed.expected_head -cnotmatch "^([a-f0-9]{40}|[a-f0-9]{64})$" -or $TopologyAllowed.manifests -isnot [array]) {return $false}
            $TopologyRootHash=Get-TopologyHash $TopologyRootPaths[$TopologyAllowed.root_index].ToLowerInvariant().Replace("\","/")
            $TopologyKey=$TopologyRootHash+"|"+$TopologyAllowed.relative_path
            if ($TopologySources.ContainsKey($TopologyKey)) {return $false}
            $TopologyFiles=New-Object "System.Collections.Generic.Dictionary[string,object]" ([StringComparer]::Ordinal)
            foreach ($TopologyFile in $TopologyAllowed.manifests) {
                if (@($TopologyFile.PSObject.Properties).Count -ne 3 -or -not (Test-TopologyRelative $TopologyFile.relative_path) -or $TopologyFile.relative_path -eq "." -or $TopologyFiles.ContainsKey($TopologyFile.relative_path) -or $TopologyFile.sha256 -cnotmatch "^[a-f0-9]{64}$" -or $TopologyFile.bytes -isnot [int] -and $TopologyFile.bytes -isnot [long]) {return $false}
                if ($TopologyFile.bytes -lt 0 -or $TopologyFile.bytes -gt $TopologyLimits.file_bytes) {return $false}
                $TopologyTotalFiles++;$TopologyTotalBytes+=$TopologyFile.bytes
                $TopologyWorkflow=$TopologyFile.relative_path -cmatch "^\.github/workflows/[^/]+\.ya?ml$"
                if (-not $TopologyWorkflow -and $TopologyFile.relative_path -cnotmatch "(^|/)(Cargo\.toml|package\.json|pyproject\.toml|go\.mod)$" -or $TopologyWorkflow -and $TopologyFile.bytes -gt $TopologyLimits.workflow_bytes -or $TopologyTotalFiles -gt $TopologyLimits.manifests_per_environment -or $TopologyTotalBytes -gt $TopologyLimits.total_bytes_per_environment) {return $false}
                $TopologyFiles.Add($TopologyFile.relative_path,$TopologyFile)
            }
            $TopologySources.Add($TopologyKey,@{checkout=$TopologyAllowed;files=$TopologyFiles})
        }
    } catch {return $false}
    return $true
}
function Test-TopologySourceBytes($Repo,[string]$Path,[byte[]]$Data) {
    $TopologyKey=$Repo.root_fingerprint+"|"+$Repo.relative_path
    if (-not $TopologySources.ContainsKey($TopologyKey)) {return $false}
    $TopologyAllowed=$TopologySources[$TopologyKey]
    if (-not $TopologyAllowed.files.ContainsKey($Path) -or $Repo.git.head -cne $TopologyAllowed.checkout.expected_head) {return $false}
    $TopologyFile=$TopologyAllowed.files[$Path]
    $TopologyHasher=[Security.Cryptography.SHA256]::Create()
    try {$TopologyHash=([BitConverter]::ToString($TopologyHasher.ComputeHash($Data))).Replace("-","").ToLowerInvariant()} finally {$TopologyHasher.Dispose()}
    return ($Data.Length -eq $TopologyFile.bytes -and $TopologyHash -ceq $TopologyFile.sha256)
}
function Confirm-TopologySources {
    $TopologySeen=New-Object "System.Collections.Generic.HashSet[string]" ([StringComparer]::Ordinal)
    $TopologyComplete=$true
    foreach ($TopologyRepo in $TopologyRepositories) {
        $TopologyKey=$TopologyRepo.root_fingerprint+"|"+$TopologyRepo.relative_path
        if (-not $TopologySources.ContainsKey($TopologyKey) -or -not $TopologySeen.Add($TopologyKey) -or $TopologyRepo.config_file_read -or @($TopologyRepo.configs).Count -gt 0) {$script:TopologySourceDenied=$true;break}
        $TopologyAllowed=$TopologySources[$TopologyKey]
        $TopologyPath=Get-TopologyUnderRoot $TopologyRootPaths[$TopologyAllowed.checkout.root_index] $TopologyRepo.relative_path
        if (-not $TopologyPath -or $TopologyRepo.git.head -cne $TopologyAllowed.checkout.expected_head -or (Invoke-TopologyGit $TopologyPath "rev-parse --verify HEAD") -cne $TopologyAllowed.checkout.expected_head) {$script:TopologySourceDenied=$true;break}
        $TopologyFiles=New-Object "System.Collections.Generic.HashSet[string]" ([StringComparer]::Ordinal)
        foreach ($TopologyFile in $TopologyRepo.manifests) {
            if (-not $TopologyFiles.Add($TopologyFile.relative_path) -or $TopologyFile.revision -cne $TopologyAllowed.checkout.expected_head -or -not (Test-TopologySourceBytes $TopologyRepo $TopologyFile.relative_path ([Text.Encoding]::UTF8.GetBytes($TopologyFile.content)))) {$script:TopologySourceDenied=$true;break}
        }
        if ($TopologyFiles.Count -ne $TopologyAllowed.files.Count) {$TopologyComplete=$false;Add-TopologyFailure "permission_denied" "source allowlist incomplete" $TopologyRepo}
    }
    if ($TopologySourceDenied -or -not $TopologySourcePolicyOk) {
        $TopologyRepositories.Clear();$TopologyCounters.repositories=0;$TopologyCounters.manifests=0;$TopologyCounters.config_files=0;$TopologyCounters.config_keys=0;$TopologyResult.progress=$null;$TopologyFailures.Clear()
        Add-TopologyFailure "permission_denied" "remote source allowlist mismatch"
    } elseif (-not $TopologyComplete -or $TopologySeen.Count -ne $TopologySources.Count) {Add-TopologyFailure "permission_denied" "source allowlist incomplete"}
}
function Test-TopologyCursor($Value,[int]$Roots) {
    if ($null -eq $Value) {return $true}
    if ($Value.version -isnot [int] -and $Value.version -isnot [long]) {return $false}
    if ($Value.next_root -isnot [int] -and $Value.next_root -isnot [long]) {return $false}
    if ($Value.frames -isnot [array]) {return $false}
    if (@($Value.PSObject.Properties).Count -ne 4 -or $Value.version -ne 1 -or $Value.fingerprint -notmatch "^[a-f0-9]{64}$" -or $null -eq $Value.next_root -or [double]$Value.next_root -ne [Math]::Truncate([double]$Value.next_root) -or $Value.next_root -lt 0 -or $Value.next_root -gt $Roots) {return $false}
    $TopologyCursorFrames=@($Value.frames)
    if ($TopologyCursorFrames.Count -gt [Math]::Min(64,$TopologyLimits.depth+1) -or ($Value.next_root -eq $Roots -and $TopologyCursorFrames.Count -gt 0)) {return $false}
    if ([Text.Encoding]::UTF8.GetByteCount((ConvertTo-Json -InputObject $Value -Compress -Depth 64)) -gt 8192) {return $false}
    for ($TopologyI=0;$TopologyI -lt $TopologyCursorFrames.Count;$TopologyI++) {
        $TopologyFrame=$TopologyCursorFrames[$TopologyI]
        if ($TopologyFrame.dir -isnot [string] -or $TopologyFrame.after -isnot [string]) {return $false}
        if (@($TopologyFrame.PSObject.Properties).Count -ne 2 -or -not (Test-TopologyRelative $TopologyFrame.dir) -or -not (Test-TopologyRelative $TopologyFrame.after $true) -or ($TopologyI -eq 0 -and $TopologyFrame.dir -ne ".")) {return $false}
        if ($TopologyFrame.dir -ne "." -and @($TopologyFrame.dir -split "/").Count -gt $TopologyLimits.depth) {return $false}
        if ($TopologyI -gt 0) {$TopologyParent=$TopologyCursorFrames[$TopologyI-1];$TopologyExpected=$TopologyParent.after;if ($TopologyParent.dir -ne ".") {$TopologyExpected=$TopologyParent.dir+"/"+$TopologyExpected};if ($TopologyFrame.dir -cne $TopologyExpected) {return $false}}
    }
    return $true
}
function Get-TopologyProgress {
    if ($TopologyRootIndex -ge $TopologyRootPaths.Count) {return $null}
    return @{version=1;fingerprint=$TopologyFingerprint;next_root=$TopologyRootIndex;frames=@($TopologyFrames | ForEach-Object {@{dir=$_.dir;after=$_.after}})}
}
function Test-TopologyFits {
    $TopologyResult.progress=Get-TopologyProgress
    return ([Text.Encoding]::UTF8.GetByteCount((ConvertTo-Json -InputObject $TopologyResult -Depth 64 -Compress))+192 -le $TopologyLimits.stdout_bytes)
}
function Get-TopologyUnderRoot([string]$Root,[string]$Relative) {
    # Reject a reparse point in any root ancestor, not only at the selected root.
    $TopologyAncestor=[IO.Path]::GetPathRoot($Root)
    foreach ($TopologyPart in @(".")+@($Root.Substring($TopologyAncestor.Length) -split "[\\/]" | Where-Object {$_ -ne ""})) {
        if ($TopologyPart -ne ".") {$TopologyAncestor=Join-Path $TopologyAncestor $TopologyPart}
        try {$TopologyAncestorItem=Get-Item -LiteralPath $TopologyAncestor -Force;if (($TopologyAncestorItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {return $null}} catch {return $null}
    }
    $TopologyPath=$Root
    foreach ($TopologyComponent in @(".")+@($Relative -split "/" | Where-Object {$_ -ne "."})) {
        if ($TopologyComponent -ne ".") {$TopologyPath=Join-Path $TopologyPath $TopologyComponent}
        try {$TopologyItem=Get-Item -LiteralPath $TopologyPath -Force;if (($TopologyItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {return $null}} catch {return $null}
    }
    $TopologyFull=[IO.Path]::GetFullPath($TopologyPath)
    if ($TopologyFull -ne $Root -and -not $TopologyFull.StartsWith($Root.TrimEnd("\")+"\",[StringComparison]::OrdinalIgnoreCase)) {return $null}
    return $TopologyFull
}
function Invoke-TopologyGit([string]$Directory,[string]$Operation) {
    if (-not (Test-TopologyTime)) {return $null}
    if (-not [IO.File]::Exists($TopologyGit)) {Add-TopologyFailure "tool_missing" "git executable unavailable";return $null}
    $TopologyCounters.children++;$TopologyProcess=New-Object Diagnostics.Process;$TopologyStart=New-Object Diagnostics.ProcessStartInfo
    $TopologyStart.FileName=$TopologyGit;$TopologyStart.Arguments="--no-optional-locks -c core.fsmonitor=false -c core.hooksPath=NUL "+$Operation;$TopologyStart.WorkingDirectory=$Directory;$TopologyStart.UseShellExecute=$false;$TopologyStart.CreateNoWindow=$true
    $TopologyStart.RedirectStandardOutput=$true;$TopologyStart.RedirectStandardError=$true;$TopologyStart.RedirectStandardInput=$true
    foreach ($TopologyEnvKey in @($TopologyStart.EnvironmentVariables.Keys)) {if ($TopologyEnvKey.StartsWith("GIT_",[StringComparison]::OrdinalIgnoreCase)) {$TopologyStart.EnvironmentVariables.Remove($TopologyEnvKey)}}
    $TopologyStart.EnvironmentVariables["GIT_CONFIG_NOSYSTEM"]="1";$TopologyStart.EnvironmentVariables["GIT_CONFIG_GLOBAL"]="NUL";$TopologyStart.EnvironmentVariables["GIT_CONFIG_SYSTEM"]="NUL"
    $TopologyStart.EnvironmentVariables["GIT_OPTIONAL_LOCKS"]="0";$TopologyStart.EnvironmentVariables["GIT_NO_LAZY_FETCH"]="1";$TopologyStart.EnvironmentVariables["GIT_TERMINAL_PROMPT"]="0";$TopologyProcess.StartInfo=$TopologyStart
    $TopologyBytes=New-Object IO.MemoryStream
    try {
        [void]$TopologyProcess.Start();$TopologyProcess.StandardInput.Close();$TopologyGitClock=[Diagnostics.Stopwatch]::StartNew();$TopologyChunk=New-Object byte[] 1024
        while ($true) {$TopologyRead=$TopologyProcess.StandardOutput.BaseStream.ReadAsync($TopologyChunk,0,$TopologyChunk.Length);while (-not $TopologyRead.Wait(50)) {if ($TopologyGitClock.Elapsed.TotalSeconds -ge 3 -or -not (Test-TopologyTime)) {$TopologyProcess.Kill();Add-TopologyFailure "timeout" "git read timed out";return $null}};$TopologyCount=$TopologyRead.Result;if ($TopologyCount -eq 0) {break};if ($TopologyBytes.Length+$TopologyCount -gt 8192) {$TopologyProcess.Kill();Add-TopologyFailure "limit_reached" "git metadata output exceeded";return $null};$TopologyBytes.Write($TopologyChunk,0,$TopologyCount)}
        if (-not $TopologyProcess.WaitForExit(500)) {$TopologyProcess.Kill();Add-TopologyFailure "timeout" "git read timed out";return $null}
        if ($TopologyProcess.ExitCode -eq 0) {return [Text.Encoding]::UTF8.GetString($TopologyBytes.ToArray()).Trim()};return $null
    } catch {Add-TopologyFailure "parse_error" "git metadata unavailable";return $null} finally {try {if (-not $TopologyProcess.HasExited) {$TopologyProcess.Kill();[void]$TopologyProcess.WaitForExit(500)}} catch {};$TopologyBytes.Dispose();$TopologyProcess.Dispose()}
}
function Get-TopologyOrigin([string]$Value) {
    if (-not $Value) {return $null};$Value=($Value -split "[?#]",2)[0]
    if ($Value.Contains("://")) {$Value=($Value -split "://",2)[1];$Value=($Value -split "@")[-1]} elseif ($Value.Contains("@") -and $Value.Contains(":")) {$Value=($Value -split "@")[-1];$Value=$Value.Replace(":","/")}
    if ($Value -notmatch "^[a-zA-Z0-9.-]+/[^\\]+$") {return $null};return $Value.TrimEnd("/") -replace "\.git$",""
}
function Get-TopologyRepo([string]$Root,[string]$Relative) {
    $TopologyParts=@();if ($Relative -ne ".") {$TopologyParts=@($Relative -split "/")}
    for ($TopologyLevel=$TopologyParts.Count;$TopologyLevel -ge 0;$TopologyLevel--) {
        $TopologyBase=".";if ($TopologyLevel -gt 0) {$TopologyBase=($TopologyParts[0..($TopologyLevel-1)] -join "/")}
        $TopologyDirectory=Get-TopologyUnderRoot $Root $TopologyBase
        if (-not $TopologyDirectory) {continue}
        $TopologyMarker=Join-Path $TopologyDirectory ".git"
        if (-not (Test-Path -LiteralPath $TopologyMarker)) {continue}
        $TopologyMarkerInfo=Get-Item -LiteralPath $TopologyMarker -Force
        if (($TopologyMarkerInfo.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {continue}
        $TopologyCacheKey=[string]$TopologyRootIndex+"|"+$TopologyBase
        if ($TopologyCache.ContainsKey($TopologyCacheKey)) {return $TopologyCache[$TopologyCacheKey]}
        if ($TopologyCounters.repositories -ge $TopologyLimits.repositories_per_environment) {Add-TopologyFailure "limit_reached" "repository budget exceeded";$script:TopologyStop=$true;return $null}
        $TopologyPrivate=Invoke-TopologyGit $TopologyDirectory "rev-parse --absolute-git-dir";$TopologyCommon=Invoke-TopologyGit $TopologyDirectory "rev-parse --path-format=absolute --git-common-dir"
        $TopologyRepo=@{root_fingerprint=(Get-TopologyHash $Root.ToLowerInvariant().Replace("\","/"));relative_path=$TopologyBase;git=@{origin=(Get-TopologyOrigin (Invoke-TopologyGit $TopologyDirectory "config --local --get remote.origin.url"));head=(Invoke-TopologyGit $TopologyDirectory "rev-parse --verify HEAD");git_dir=$(if ($TopologyPrivate) {Get-TopologyHash $TopologyPrivate});common_dir=$(if ($TopologyCommon) {Get-TopologyHash $TopologyCommon});is_worktree=[bool]($TopologyPrivate -and $TopologyCommon -and $TopologyPrivate -ne $TopologyCommon)};manifests=(New-Object System.Collections.ArrayList);configs=@();failures=(New-Object System.Collections.ArrayList)}
        if (-not $TopologyPrivate -or -not $TopologyCommon) {Add-TopologyFailure "parse_error" "repository metadata unavailable" $TopologyRepo}
        if ($TopologyRootIndex -eq $TopologyInitialRoot) {for ($TopologyJ=0;$TopologyJ -lt $TopologyInitialFrames.Count;$TopologyJ++) {$TopologyInitial=$TopologyInitialFrames[$TopologyJ];if ($TopologyInitial.dir -ceq $TopologyBase -and ($TopologyInitial.after -ne "" -or $TopologyJ -lt $TopologyInitialFrames.Count-1)) {Add-TopologyFailure "limit_reached" "continued-fragment" $TopologyRepo}}}
        [void]$TopologyRepositories.Add($TopologyRepo);$TopologyCounters.repositories++
        if (-not (Test-TopologyFits)) {$TopologyRepositories.RemoveAt($TopologyRepositories.Count-1);$TopologyCounters.repositories--;Add-TopologyFailure "limit_reached" "stdout metadata budget exceeded";$script:TopologyStop=$true;return $null}
        $TopologyCache[$TopologyCacheKey]=$TopologyRepo;[void]$TopologyActive.Add($TopologyCacheKey);return $TopologyRepo
    }
    return $null
}
function Read-TopologyFile([string]$Path,[long]$Maximum,$Repo=$null) {
    if (-not (Test-TopologyTime)) {$script:TopologyStop=$true;return $null}
    $TopologyBudget=[Math]::Max(0,[long]$TopologyLimits.total_bytes_per_environment-$TopologyCounters.bytes)
    try {
        $TopologyItem=Get-Item -LiteralPath $Path -Force
        if ($TopologyItem.PSIsContainer -or ($TopologyItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {Add-TopologyFailure "unsupported" "non regular input rejected" $Repo;return $null}
        if ($TopologyItem.Length -gt $Maximum) {Add-TopologyFailure "limit_reached" "file budget exceeded; narrow root or raise limit" $Repo;return $null}
        if ($TopologyItem.Length -gt $TopologyBudget) {if ($TopologyCounters.bytes -gt 0) {Add-TopologyFailure "limit_reached" "total read budget exceeded";$script:TopologyStop=$true} else {Add-TopologyFailure "limit_reached" "stalled_directory: file exceeds total budget; narrow root or raise limit" $Repo};return $null}
        $TopologyStream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
        try {$TopologyBuffer=New-Object byte[] ([int]$TopologyItem.Length);$TopologyOffset=0;while ($TopologyOffset -lt $TopologyBuffer.Length) {$TopologyCount=$TopologyStream.Read($TopologyBuffer,$TopologyOffset,$TopologyBuffer.Length-$TopologyOffset);if ($TopologyCount -eq 0) {break};$TopologyOffset+=$TopologyCount};$TopologyCounters.files++;$TopologyCounters.bytes+=$TopologyOffset;if ($TopologyOffset -ne $TopologyItem.Length -or $TopologyStream.Length -ne $TopologyItem.Length) {Add-TopologyFailure "parse_error" "input changed during read" $Repo;return $null};return ,$TopologyBuffer} finally {$TopologyStream.Dispose()}
    } catch {Add-TopologyFailure "permission_denied" "input unreadable" $Repo;return $null}
}
$TopologyIdentity=[Environment]::MachineName.ToLowerInvariant() -replace "\.local$",""
$script:TopologyResult=@{schema="katala_topology_capture.v1";scope_id=$TopologyRequest.scope_id;adapter="remote";observed_at_ms=$TopologyRequest.observed_at_ms;limits=$TopologyLimits;target_identity=(Get-TopologyHash $TopologyIdentity);runtime=@{kind="native";platform="windows";kernel_wsl=$false;distro=$null};repositories=$TopologyRepositories;failures=$TopologyFailures;counters=$TopologyCounters;progress=$null}
$TopologyRuntimeOk=($TopologyRequest.expected_runtime -ne "wsl")
if ($TopologyRequest.expected_identity_hash -and $TopologyRequest.expected_identity_hash -ne $TopologyResult.target_identity) {$TopologyRuntimeOk=$false}
$script:TopologyRootPaths=New-Object System.Collections.ArrayList;$TopologyRootStamps=New-Object System.Collections.ArrayList
foreach ($TopologyRawRoot in $TopologyRequest.roots) {
    $TopologyRoot=[IO.Path]::GetFullPath($TopologyRawRoot)
    if ($TopologyRoot.Length -gt 3) {$TopologyRoot=$TopologyRoot.TrimEnd("\")}
    [void]$TopologyRootPaths.Add($TopologyRoot)
    try {$TopologyItem=Get-Item -LiteralPath $TopologyRoot -Force;[void]$TopologyRootStamps.Add(@($TopologyRoot.ToLowerInvariant().Replace("\","/"),$TopologyItem.CreationTimeUtc.Ticks,$TopologyItem.LastWriteTimeUtc.Ticks))} catch {[void]$TopologyRootStamps.Add(@($TopologyRoot,$null))}
}
$TopologyFingerprintMaterial=[ordered]@{version=1;roots=@($TopologyRootStamps);skip=@($TopologySkip | Sort-Object);parser="katala-topology-parser.v1";limits=$TopologyLimits;configs=@($TopologyRequest.configs);source_allowlist=@($TopologyRequest.source_allowlist);hash_mode="hash_json_typed_v1"}
$script:TopologyFingerprint=Get-TopologyHash (ConvertTo-Json -InputObject $TopologyFingerprintMaterial -Depth 64 -Compress)
$TopologySourcePolicyOk=Test-TopologySourcePolicy
$TopologyCursor=$TopologyRequest.cursor
$TopologyCursorOk=Test-TopologyCursor $TopologyCursor $TopologyRootPaths.Count
if (-not $TopologySourcePolicyOk) {Add-TopologyFailure "permission_denied" "explicit source allowlist required"}
elseif (-not $TopologyRuntimeOk) {Add-TopologyFailure "permission_denied" "runtime identity mismatch"}
elseif (-not $TopologyCursorOk) {Add-TopologyFailure "permission_denied" "invalid continuation cursor"}
elseif ($TopologyRootPaths.Count -eq 0 -or $TopologyRootPaths.Count -gt $TopologyLimits.roots_per_environment) {Add-TopologyFailure "unsupported" "invalid configured root count"}
else {
    if ($TopologyCursor -and $TopologyCursor.fingerprint -ne $TopologyFingerprint) {Add-TopologyFailure "unsupported" "continuation cursor invalidated";$TopologyCursor=$null}
    if ($TopologyCursor) {$script:TopologyRootIndex=[int]$TopologyCursor.next_root;foreach ($TopologyFrame in $TopologyCursor.frames) {[void]$TopologyFrames.Add(@{dir=[string]$TopologyFrame.dir;after=[string]$TopologyFrame.after})}}
    $script:TopologyInitialRoot=$TopologyRootIndex;$script:TopologyInitialFrames=@($TopologyFrames | ForEach-Object {@{dir=$_.dir;after=$_.after}})
    while ($TopologyRootIndex -lt $TopologyRootPaths.Count -and -not $TopologyStop) {
        $TopologyRoot=$TopologyRootPaths[$TopologyRootIndex]
        if (-not (Get-TopologyUnderRoot $TopologyRoot ".")) {Add-TopologyFailure "permission_denied" "root missing unreadable or symlink";$script:TopologyRootIndex++;$TopologyFrames.Clear();continue}
        if ($TopologyFrames.Count -eq 0) {[void]$TopologyFrames.Add(@{dir=".";after=""})}
        while ($TopologyFrames.Count -gt 0 -and -not $TopologyStop) {
            if (-not (Test-TopologyTime)) {$script:TopologyStop=$true;break}
            $TopologyCurrent=$TopologyFrames[$TopologyFrames.Count-1];$TopologyRelative=$TopologyCurrent.dir;$TopologyDirectory=Get-TopologyUnderRoot $TopologyRoot $TopologyRelative
            if (-not $TopologyDirectory) {Add-TopologyFailure "permission_denied" "continuation directory missing or symlink";$script:TopologyStop=$true;break}
            $TopologyRepo=Get-TopologyRepo $TopologyRoot $TopologyRelative
            if ($TopologyStop) {break}
            $TopologyListingKey=[string]$TopologyRootIndex+"|"+$TopologyRelative
            if ($TopologyDirectoryCache.ContainsKey($TopologyListingKey)) {$TopologySorted=$TopologyDirectoryCache[$TopologyListingKey]}
            else {
                $TopologyNames=New-Object System.Collections.ArrayList
                try {
                    foreach ($TopologyPath in [IO.Directory]::EnumerateFileSystemEntries($TopologyDirectory)) {
                        if (-not (Test-TopologyTime)) {$script:TopologyStop=$true;break}
                        if ($TopologyCounters.entries -ge $TopologyLimits.entries_per_environment) {Add-TopologyFailure "limit_reached" "entry budget exceeded";$script:TopologyStop=$true;if ($TopologyNames.Count -ge $TopologyLimits.entries_per_environment) {Add-TopologyFailure "limit_reached" "stalled_directory: narrow root or raise entry limit";$script:TopologyStalled=$true};break}
                        $TopologyCounters.entries++;[void]$TopologyNames.Add([IO.Path]::GetFileName($TopologyPath))
                    }
                } catch {Add-TopologyFailure "permission_denied" "directory unreadable" $TopologyRepo;$TopologyFrames.RemoveAt($TopologyFrames.Count-1);continue}
                if ($TopologyStop) {break}
                $TopologySorted=[string[]]$TopologyNames;[Array]::Sort($TopologySorted,[StringComparer]::Ordinal)
                $TopologyDirectoryCache[$TopologyListingKey]=$TopologySorted
            }
            $TopologyDescended=$false
            foreach ($TopologyName in $TopologySorted) {
                if ([StringComparer]::Ordinal.Compare($TopologyName,$TopologyCurrent.after) -le 0) {continue}
                $TopologyPath=Join-Path $TopologyDirectory $TopologyName
                try {$TopologyItem=Get-Item -LiteralPath $TopologyPath -Force} catch {Add-TopologyFailure "permission_denied" "entry unreadable" $TopologyRepo;$TopologyCurrent.after=$TopologyName;continue}
                if (($TopologyItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {$TopologyCurrent.after=$TopologyName;continue}
                if ($TopologyItem.PSIsContainer) {
                    if ($TopologySkip -contains $TopologyName) {$TopologyCurrent.after=$TopologyName;continue}
                    if ($TopologyFrames.Count-1 -ge $TopologyLimits.depth -or $TopologyFrames.Count -ge 64) {Add-TopologyFailure "limit_reached" "depth budget exceeded" $TopologyRepo;$TopologyCurrent.after=$TopologyName;continue}
                    $TopologyPrevious=$TopologyCurrent.after;$TopologyCurrent.after=$TopologyName;$TopologyChild=$TopologyName;if ($TopologyRelative -ne ".") {$TopologyChild=$TopologyRelative+"/"+$TopologyName};[void]$TopologyFrames.Add(@{dir=$TopologyChild;after=""})
                    if ([Text.Encoding]::UTF8.GetByteCount((ConvertTo-Json -InputObject (Get-TopologyProgress) -Depth 64 -Compress)) -gt 8192 -or -not (Test-TopologyFits)) {$TopologyFrames.RemoveAt($TopologyFrames.Count-1);$TopologyCurrent.after=$TopologyPrevious;Add-TopologyFailure "limit_reached" "stalled_directory: cursor metadata budget; narrow root";$script:TopologyStop=$true;$script:TopologyStalled=$true}
                    $TopologyDescended=$true;break
                }
                if ($null -ne $TopologyRepo) {
                    $TopologyBase=[IO.Path]::GetFullPath((Join-Path $TopologyRoot $TopologyRepo.relative_path));$TopologyRepoRelative=$TopologyPath.Substring($TopologyBase.TrimEnd("\").Length).TrimStart("\","/").Replace("\","/")
                    $TopologyKind=switch ($TopologyName) {"Cargo.toml" {"cargo"} "package.json" {"node"} "pyproject.toml" {"python"} "go.mod" {"go"} default {$null}}
                    if ($TopologyRepoRelative -match "^\.github/workflows/[^/]+\.ya?ml$") {$TopologyKind="workflow"}
                    if ($TopologyKind) {
                        if ($TopologyCounters.manifests -ge $TopologyLimits.manifests_per_environment) {Add-TopologyFailure "limit_reached" "manifest budget exceeded";$script:TopologyStop=$true;break}
                        $TopologyMaximum=[long]$TopologyLimits.file_bytes;if ($TopologyKind -eq "workflow") {$TopologyMaximum=[Math]::Min($TopologyMaximum,$TopologyLimits.workflow_bytes)}
                        $TopologyData=Read-TopologyFile $TopologyPath $TopologyMaximum $TopologyRepo
                        if ($TopologyStop) {break}
                        if ($null -ne $TopologyData) {
                            if (-not (Test-TopologySourceBytes $TopologyRepo $TopologyRepoRelative $TopologyData)) {$script:TopologySourceDenied=$true;$TopologyCurrent.after=$TopologyName;continue}
                            try {$TopologyUtf8=New-Object Text.UTF8Encoding($false,$true);$TopologyContent=$TopologyUtf8.GetString($TopologyData)} catch {Add-TopologyFailure "parse_error" "manifest is not utf-8" $TopologyRepo;$TopologyCurrent.after=$TopologyName;continue}
                            [void]$TopologyRepo.manifests.Add(@{relative_path=$TopologyRepoRelative;kind=$TopologyKind;content=$TopologyContent;content_sha256=(Get-TopologyHash $TopologyContent);revision=$TopologyRepo.git.head});$TopologyCounters.manifests++
                            if (-not (Test-TopologyFits)) {$TopologyRepo.manifests.RemoveAt($TopologyRepo.manifests.Count-1);$TopologyCounters.manifests--;if ($TopologyCounters.manifests -gt 0) {Add-TopologyFailure "limit_reached" "stdout byte budget exceeded";$script:TopologyStop=$true;break};Add-TopologyFailure "limit_reached" "stalled_directory: manifest stdout budget; narrow root or raise limit" $TopologyRepo}
                        }
                    }
                }
                $TopologyCurrent.after=$TopologyName
            }
            if ($TopologyStop) {break}
            if (-not $TopologyDescended) {[void]$TopologyActive.Remove([string]$TopologyRootIndex+"|"+$TopologyRelative);$TopologyFrames.RemoveAt($TopologyFrames.Count-1)}
        }
        if ($TopologyFrames.Count -eq 0) {$script:TopologyRootIndex++}
    }
    foreach ($TopologyKey in $TopologyActive) {Add-TopologyFailure "limit_reached" "continued-fragment" $TopologyCache[$TopologyKey]}
    $TopologyResult.progress=Get-TopologyProgress
    $TopologyConfigIndex=0
    $TopologyConfigKeyAttempts=0
    foreach ($TopologySelection in $TopologyRequest.configs) {
        if ($TopologyConfigIndex++ -ge $TopologyLimits.manifests_per_environment) {Add-TopologyFailure "limit_reached" "configuration file budget exceeded";break}
        if (-not (Test-TopologyTime)) {break}
        if (-not [IO.Path]::IsPathRooted($TopologySelection.path) -or ($TopologySelection.format -and $TopologySelection.format -ne "json")) {Add-TopologyFailure "unsupported" "configuration format or path unsupported";continue}
        if ($TopologyCounters.repositories -ge $TopologyLimits.repositories_per_environment) {Add-TopologyFailure "limit_reached" "configuration repository budget exceeded";break}
        $TopologyConfigPath=[IO.Path]::GetFullPath($TopologySelection.path);$TopologyPathId=Get-TopologyHash $TopologyConfigPath.ToLowerInvariant().Replace("\","/")
        $TopologyConfigRepo=@{root_fingerprint=(Get-TopologyHash ($TopologyRequest.scope_id+"|configuration|"+$TopologyPathId));relative_path="configuration-"+$TopologyPathId;git=@{origin=$null;head=$null;git_dir=$null;common_dir=$null;is_worktree=$false};manifests=@();configs=(New-Object System.Collections.ArrayList);config_file_read=$false;failures=(New-Object System.Collections.ArrayList)}
        $TopologyData=Read-TopologyFile $TopologyConfigPath $TopologyLimits.file_bytes $TopologyConfigRepo
        if ($null -eq $TopologyData) {Add-TopologyFailure "parse_error" "selected configuration unavailable" $TopologyConfigRepo}
        else {$TopologyConfigRepo.config_file_read=$true;$TopologyCounters.config_files++;try {
            $TopologyValue=ConvertFrom-Json -InputObject ([Text.Encoding]::UTF8.GetString($TopologyData));foreach ($TopologyKey in $TopologySelection.keys) {
                if ($TopologyConfigKeyAttempts -ge $TopologyLimits.manifests_per_environment) {Add-TopologyFailure "limit_reached" "configuration key budget exceeded";Add-TopologyFailure "limit_reached" "configuration key budget exceeded" $TopologyConfigRepo;break}
                $TopologyConfigKeyAttempts++
                if (($TopologyKey.mode -and $TopologyKey.mode -notin @("hash","hash_json_typed_v1")) -or ($TopologyKey.pointer -ne "" -and -not $TopologyKey.pointer.StartsWith("/"))) {Add-TopologyFailure "permission_denied" "literal configuration rejected" $TopologyConfigRepo;continue}
                $TopologySelected=$TopologyValue
                foreach ($TopologyComponentRaw in ($TopologyKey.pointer -split "/" | Select-Object -Skip 1)) {
                    $TopologyComponent=$TopologyComponentRaw.Replace("~1","/").Replace("~0","~")
                    if ($TopologySelected -is [array]) {if ($TopologyComponent -notmatch "^(0|[1-9][0-9]*)$" -or [long]$TopologyComponent -ge $TopologySelected.Count) {throw "missing"};$TopologySelected=$TopologySelected[[int]$TopologyComponent]}
                    else {$TopologyProperty=$TopologySelected.PSObject.Properties[$TopologyComponent];if ($null -eq $TopologyProperty -or $TopologyProperty.Name -cne $TopologyComponent) {throw "missing"};$TopologySelected=$TopologyProperty.Value}
                }
                $TopologyCanonical=Get-TopologyCanonical $TopologySelected
                [void]$TopologyConfigRepo.configs.Add(@{relative_path=$TopologyPathId;key=$TopologyKey.pointer;value_hash=(Get-TopologyHash $TopologyCanonical);mode="hash_json_typed_v1"})
                $TopologyCounters.config_keys++
            }
        } catch {Add-TopologyFailure "parse_error" "selected configuration unavailable" $TopologyConfigRepo}}
        [void]$TopologyRepositories.Add($TopologyConfigRepo)
        $TopologyCounters.repositories++
        if (-not (Test-TopologyFits)) {$TopologyRemoved=$TopologyRepositories[$TopologyRepositories.Count-1];$TopologyRepositories.RemoveAt($TopologyRepositories.Count-1);$TopologyCounters.repositories--;if ($TopologyRemoved.config_file_read) {$TopologyCounters.config_files--};$TopologyCounters.config_keys-=$TopologyRemoved.configs.Count;Add-TopologyFailure "limit_reached" "configuration stdout budget exceeded";break}
    }
}
Confirm-TopologySources
$TopologyCounters.duration_ms=$TopologyClock.ElapsedMilliseconds
$TopologyEncoded=ConvertTo-Json -InputObject $TopologyResult -Depth 64 -Compress
if ([Text.Encoding]::UTF8.GetByteCount($TopologyEncoded) -gt $TopologyLimits.stdout_bytes) {$TopologyRepositories.Clear();$TopologyCounters.repositories=0;$TopologyCounters.manifests=0;$TopologyCounters.config_files=0;$TopologyCounters.config_keys=0;$TopologyResult.progress=$null;$TopologyFailures.Clear();Add-TopologyFailure "limit_reached" "stalled_directory: stdout metadata budget; raise limit";$TopologyEncoded=ConvertTo-Json -InputObject $TopologyResult -Depth 64 -Compress}
[Console]::Out.WriteLine($TopologyEncoded)
