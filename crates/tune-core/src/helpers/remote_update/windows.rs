//! Fixed PS5.1 byte-stream bootstrap. No environment-selected destination.
use super::*;
pub(super) const GLUE: &[u8] = include_bytes!("windows_glue.ps1");
pub(super) const GLUE_LENGTH: usize = 45261;
pub(super) const GLUE_HASH: &str = "52a0b6ec50910882815c30e0ad711a2bb83335fff9e109c0805df90cb36e2416";
pub(super) fn supported() -> bool {
    cfg!(all(target_os = "windows", target_arch = "x86_64"))
}
pub(super) fn preflight(node: &Node, payload: RemoteUpdatePayload<'_>) -> Result<(), Refusal> {
    if !supported()
        || node.os != "windows"
        || node.local
        || payload.request.target.architecture != RemoteArchitecture::X64
        || payload.build_target() != "x86_64-pc-windows-msvc"
    {
        return Err(Refusal::Target);
    }
    observation::remaining(payload.request)?;
    payload.destination_observation().ok_or(Refusal::InvalidRequest)?.check(payload.request, node)
}
pub(super) fn bootstrap() -> String {
    format!(
        r#"$ErrorActionPreference="Stop";$ProgressPreference="SilentlyContinue";$WarningPreference="SilentlyContinue";$InformationPreference="SilentlyContinue";$s=$null;try{{$s=[Console]::OpenStandardInput();function R([int]$n){{$a=New-Object byte[] $n;$i=0;while($i-lt$n){{$r=$s.Read($a,$i,$n-$i);if(!$r){{throw 0}};$i+=$r}};return ,$a}};$l="";do{{$c=$s.ReadByte();if($c-lt0-or$l.Length-ge96){{throw 0}};$l+=[char]$c}}while($c-ne10);if($l-cnotmatch"\AKTU2 ([1-9][0-9]*) ([1-9][0-9]*) (0|[1-9][0-9]*)`n\z"){{throw 0}};$h=[int]$Matches[1];$g=[int]$Matches[2];$b=[long]$Matches[3];if($h-gt16384-or$g-ne{length}-or$b-gt268435456){{throw 0}};$hb=R $h;$q=[Diagnostics.Stopwatch]::GetTimestamp();$f=[Diagnostics.Stopwatch]::Frequency;$gb=R $g;$sha=[Security.Cryptography.SHA256]::Create();try{{$hs=[BitConverter]::ToString($sha.ComputeHash($gb)).Replace("-","").ToLowerInvariant()}}finally{{$sha.Dispose()}};if($hs-cne"{hash}"-or$gb[0]-ne239-or$gb[1]-ne187-or$gb[2]-ne191){{throw 0}};for($i=3;$i-lt$g;$i++){{if($gb[$i]-gt127){{throw 0}}}};. ([ScriptBlock]::Create([Text.Encoding]::ASCII.GetString($gb,3,$g-3)));$r=Invoke-Ktu2 $s $hb $b $env:KTU2_BASH_HOME $env:KTU2_BASH_NATIVE $HOME $env:USERPROFILE $q $f;exit $r}}catch{{if($s){{try{{$a=New-Object byte[] 65536;while($s.Read($a,0,$a.Length)-gt0){{}}}}catch{{}}}};exit 2}}"#,
        length = GLUE_LENGTH,
        hash = GLUE_HASH
    )
}
pub(super) fn launcher() -> String {
    launcher_for(&bootstrap())
}
fn launcher_for(bootstrap: &str) -> String {
    let encoded: Vec<u8> = bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        r#"export LC_ALL=C.UTF-8;h=$(LC_ALL=C;[ ${{#HOME}} -ge 1 ]&&[ ${{#HOME}} -le 256 ]||exit 2;set -o pipefail;printf %s "$HOME" </dev/null|/usr/bin/od -An -v -tx1|/usr/bin/tr -d ' \n')||exit 2;[ ${{#h}} -ge 2 ]&&[ ${{#h}} -le 512 ]||exit 2;n=$(set -o pipefail;/usr/bin/cygpath -w -- "$HOME" </dev/null 2>/dev/null|{{ /usr/bin/head -c 514;r=$?;/usr/bin/cat >/dev/null||exit 2;exit $r; }};r=$?;printf '.%s' "$r" </dev/null);[[ $n == *$'\n'.0 ]]||exit 2;n=${{n%$'\n'.0}};[[ $n != *$'\n'* && $n != *$'\r'* ]]||exit 2;n=$(printf %s "$n" </dev/null|/usr/bin/od -An -v -tx1|/usr/bin/tr -d ' \n');[ ${{#n}} -ge 2 ]&&[ ${{#n}} -le 1024 ]||exit 2;export KTU2_BASH_HOME=$h KTU2_BASH_NATIVE=$n;exec powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}"#,
        crate::logs::base64(&encoded)
    )
}
impl OwnedSshUpdateTransport {
    pub fn windows() -> Result<Self, Refusal> {
        if !supported() {
            return Err(Refusal::Target);
        }
        Ok(ssh::windows_native())
    }
}

#[cfg(test)]
pub(super) fn fixture_bootstrap(source: &str, entry: &str) -> String {
    let encoded = crate::logs::base64(source.as_bytes());
    let replacement = format!(
        r#"Add-Type -TypeDefinition ($script:Ktu2Bridge+[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("{encoded}"))) -ErrorAction Stop | Out-Null;$r={entry};exit $r"#
    );
    bootstrap().replace("$r=Invoke-Ktu2 $s $hb $b $env:KTU2_BASH_HOME $env:KTU2_BASH_NATIVE $HOME $env:USERPROFILE $q $f;exit $r", &replacement)
}
#[cfg(test)]
pub(super) fn fixture_launcher(bootstrap: &str) -> String {
    launcher_for(bootstrap)
}
