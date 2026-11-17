//! Move-only native observation. Raw account/namespace data never enters facts.
use super::{
    protocol::{BeforeImageFact, SamplerHash},
    *,
};

#[derive(Debug)]
pub struct RemoteDestinationObservation {
    binding: Binding,
    pub(super) context: SamplerHash,
    pub(super) snapshot: MacSnapshot,
    origin: NativeMac,
}
#[derive(Debug)]
struct NativeMac;
#[derive(Debug, PartialEq, Eq)]
struct Binding {
    node: Vec<u8>,
    architecture: RemoteArchitecture,
    source: (String, String, String, usize, String),
    token: AttemptToken,
    attempt: u64,
    start: u64,
    expires: Instant,
}
fn node_bytes(node: &Node) -> Vec<u8> {
    serde_json::to_vec(&(&node.id, &node.alias, &node.os, node.local, node.shared, &node.raw)).expect("Node JSON tuple")
}
impl Binding {
    fn new(request: &RemoteUpdateRequest) -> Self {
        let p = &request.prepared;
        Self {
            node: node_bytes(&request.target.node),
            architecture: request.target.architecture,
            source: (p.version().into(), p.build_target().into(), p.manifest_sha256().into(), p.remote_bytes().len(), p.sampler_sha256().into()),
            token: request.token,
            attempt: request.attempt_id,
            start: request.start_seq,
            expires: request.expires,
        }
    }
}
impl RemoteDestinationObservation {
    pub(super) fn native(request: &RemoteUpdateRequest, context: SamplerHash, snapshot: MacSnapshot) -> Self {
        Self { binding: Binding::new(request), context, snapshot, origin: NativeMac }
    }
    pub(super) fn check(&self, request: &RemoteUpdateRequest, node: &Node) -> Result<(), Refusal> {
        let NativeMac = self.origin;
        if Instant::now() >= self.binding.expires {
            return Err(Refusal::Expired);
        }
        if self.binding != Binding::new(request) || self.binding.node != node_bytes(node) {
            return Err(Refusal::InvalidRequest);
        }
        Ok(())
    }
}
pub(super) fn remaining(request: &RemoteUpdateRequest) -> Result<u64, Refusal> {
    let ms = request.expires.checked_duration_since(Instant::now()).ok_or(Refusal::Expired)?.as_millis();
    if ms == 0 {
        return Err(Refusal::Expired);
    }
    if ms > 120_000 {
        return Err(Refusal::InvalidRequest);
    }
    Ok(ms as u64)
}
pub(super) fn digest(domain: &[u8], fields: &[&[u8]]) -> SamplerHash {
    let mut bytes = domain.to_vec();
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    SamplerHash::parse(&crate::db::sha256_hex(&bytes)).unwrap()
}
pub(super) fn inspect_context(request: &RemoteUpdateRequest, ttl: u64) -> SamplerHash {
    let b = Binding::new(request);
    let arch = match b.architecture {
        RemoteArchitecture::Arm64 => "arm64",
        RemoteArchitecture::X64 => "x64",
    };
    digest(
        b"KTU2-inspect\0",
        &[
            &b.node,
            arch.as_bytes(),
            b.token.as_str().as_bytes(),
            b.attempt.to_string().as_bytes(),
            b.start.to_string().as_bytes(),
            b.source.0.as_bytes(),
            b.source.1.as_bytes(),
            b.source.2.as_bytes(),
            b.source.3.to_string().as_bytes(),
            b.source.4.as_bytes(),
            ttl.to_string().as_bytes(),
        ],
    )
}
pub(super) fn place_context(observation: &RemoteDestinationObservation) -> SamplerHash {
    digest(b"KTU2-place\0", &[observation.context.raw(), observation.snapshot.record.as_bytes()])
}
#[derive(Debug)]
pub(super) struct MacSnapshot {
    pub record: String,
    pub before: BeforeImageFact,
}
fn decimal<T: std::str::FromStr + ToString>(s: &str) -> Option<T> {
    let v: T = s.parse().ok()?;
    (v.to_string() == s).then_some(v)
}
impl MacSnapshot {
    pub(super) fn parse(record: &str) -> Option<Self> {
        if record.len() > 3072 || !record.is_ascii() {
            return None;
        }
        let fields: Vec<_> = record.split('|').collect();
        let [uid, home, "darwin", "arm64", lease, parents, leaf] = fields.as_slice() else {
            return None;
        };
        decimal::<u32>(uid)?;
        decimal::<u64>(lease)?;
        if home.is_empty() || home.len() > 768 || home.len() % 2 != 0 || !home.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return None;
        }
        let raw: Vec<u8> = home.as_bytes().as_chunks::<2>().0.iter().map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect();
        if raw.first() != Some(&b'/') || raw.contains(&0) {
            return None;
        }
        let components: Vec<_> = raw[1..].split(|b| *b == b'/').collect();
        if components.iter().any(|p| p.is_empty() || *p == b"." || *p == b"..") {
            return None;
        }
        let ps: Vec<_> = parents.split(';').collect();
        if ps.len() != components.len() + 3 || ps.len() > 24 {
            return None;
        }
        for parent in ps {
            let p: Vec<_> = parent.split(',').collect();
            let [dev, ino, uid, mode] = p.as_slice() else {
                return None;
            };
            decimal::<u64>(dev)?;
            decimal::<u64>(ino)?;
            decimal::<u32>(uid)?;
            decimal::<u32>(mode)?;
        }
        let before = if *leaf == "M" {
            BeforeImageFact::Missing
        } else {
            let p: Vec<_> = leaf.split(',').collect();
            let ["P", dev, ino, uid, size, mode, mt, ct, "1", hash] = p.as_slice() else {
                return None;
            };
            decimal::<u64>(dev)?;
            decimal::<u64>(ino)?;
            decimal::<u32>(uid)?;
            decimal::<u32>(mode)?;
            let size = decimal::<u64>(size)?;
            if size == 0 || size > super::protocol::SAMPLER_LIMIT as u64 {
                return None;
            }
            decimal::<i128>(mt)?;
            decimal::<i128>(ct)?;
            BeforeImageFact::Present(SamplerHash::parse(hash)?)
        };
        Some(Self { record: record.into(), before })
    }
}
