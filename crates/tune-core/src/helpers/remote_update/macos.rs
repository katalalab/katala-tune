//! Fixed header-first Python bootstrap and the opt-in native authority boundary.
use super::{
    observation,
    protocol::{ExchangeMode, Image},
    ssh, *,
};

pub(super) const GLUE: &[u8] = include_bytes!("macos_glue.py");
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectFailure {
    Unsupported,
    Account,
    Unsafe,
    Io,
    Expired,
}
impl InspectFailure {
    pub(super) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Unsupported" => Self::Unsupported,
            "Account" => Self::Account,
            "Unsafe" => Self::Unsafe,
            "Io" => Self::Io,
            "Expired" => Self::Expired,
            _ => return None,
        })
    }
}
#[derive(Debug)]
pub struct RemoteInspectError {
    pub reason: InspectFailure,
    pub local_completion: bool,
}
fn error(reason: InspectFailure) -> RemoteInspectError {
    RemoteInspectError { reason, local_completion: true }
}
pub(super) fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}
pub(super) fn preflight(node: &Node, payload: RemoteUpdatePayload<'_>) -> Result<(), Refusal> {
    if !supported() || node.os != "macos" || node.local || payload.request.target.architecture != RemoteArchitecture::Arm64 {
        return Err(Refusal::Target);
    }
    observation::remaining(payload.request)?;
    payload.destination_observation().ok_or(Refusal::InvalidRequest)?.check(payload.request, node)
}
pub(super) fn bootstrap() -> String {
    bootstrap_entry("scope[\"run_production\"](stream, header, b)")
}
fn bootstrap_entry(entry: &str) -> String {
    format!(
        r#"import sys,hashlib,re
stream=sys.stdin.buffer
line=stream.readline(97)
if len(line)>96 or re.fullmatch(rb'KTU2 (0|[1-9][0-9]*) (0|[1-9][0-9]*) (0|[1-9][0-9]*)\n',line) is None: raise SystemExit(2)
h,g,b=map(int,line[5:-1].split(b' '))
if not (0<h<=16384 and g=={length} and g<=131072 and b<=268435456): raise SystemExit(2)
def exact(n):
    chunks=[]
    while n:
        part=stream.read(min(n,65536))
        if not part: raise SystemExit(2)
        chunks.append(part);n-=len(part)
    return b''.join(chunks)
header=exact(h)
glue=exact(g)
if hashlib.sha256(glue).hexdigest()!="{hash}": raise SystemExit(2)
scope={{}}
exec(compile(glue,"<fixed-native-glue>","exec"),scope)
{entry}
"#,
        length = GLUE.len(),
        hash = crate::db::sha256_hex(GLUE)
    )
}
pub(super) fn launcher() -> String {
    format!("exec python3 -I -c '{}'", bootstrap().replace('\'', "'\"'\"'"))
}
impl OwnedSshUpdateTransport {
    pub fn macos() -> Result<Self, Refusal> {
        if !supported() {
            return Err(Refusal::Target);
        }
        Ok(ssh::native_transport())
    }
    pub async fn inspect(&self, engine: &Engine, request: &RemoteUpdateRequest) -> Result<RemoteDestinationObservation, RemoteInspectError> {
        if !self.is_native()
            || !supported()
            || request.target.node.os != "macos"
            || request.target.architecture != RemoteArchitecture::Arm64
            || request.target.node.local
        {
            return Err(error(InspectFailure::Unsupported));
        }
        if request.observation.is_some() {
            return Err(error(InspectFailure::Unsafe));
        }
        let (cfg, _) = monitor::controlled_inputs(engine).map_err(|_| error(InspectFailure::Io))?;
        if !request.target.matches(&cfg) {
            return Err(error(InspectFailure::Unsafe));
        }
        let ttl = observation::remaining(request).map_err(|_| error(InspectFailure::Expired))?;
        let context = observation::inspect_context(request, ttl);
        let payload = request.payload();
        let empty_hash = crate::db::sha256_hex(b"");
        let mut image = Image::payload(&payload, context, GLUE);
        image.mode = ExchangeMode::Inspect;
        image.bytes = b"";
        image.hash = &empty_hash;
        image.ttl_ms = ttl;
        let result = self.execute_image(&request.target.node, &image).await;
        if Instant::now() >= request.expires {
            return Err(error(InspectFailure::Expired));
        }
        if !result.reply.local_settled || !result.reply.response_complete || result.reply.code != Some(0) {
            return Err(error(result.failure.unwrap_or(InspectFailure::Io)));
        }
        let snapshot = result.observed.ok_or_else(|| error(result.failure.unwrap_or(InspectFailure::Unsafe)))?;
        Ok(RemoteDestinationObservation::native(request, context, snapshot))
    }
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
pub(super) fn fixture_bootstrap(wrapper: &str) -> String {
    // Only this cfg(test) renderer can select a noncanonical entry. Product
    // launcher contains no root argument, fixture ENV or configurable source.
    let encoded = serde_json::to_string(wrapper).unwrap();
    bootstrap_entry(&format!("exec({encoded},scope)\nscope[\"run_fixture\"](stream, header, b)"))
}
