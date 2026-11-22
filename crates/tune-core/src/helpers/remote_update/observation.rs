//! Move-only native observation. Raw account/namespace data never enters facts.
use super::{
    protocol::{BeforeImageFact, SamplerHash},
    *,
};

#[derive(Debug)]
pub struct RemoteDestinationObservation {
    binding: Binding,
    pub(super) context: SamplerHash,
    pub(super) snapshot: DestinationSnapshot,
    origin: NativeOrigin,
}
#[derive(Debug)]
enum NativeOrigin {
    NativeMac,
    NativeWindows,
}
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
    pub(super) fn native(request: &RemoteUpdateRequest, context: SamplerHash, snapshot: impl Into<DestinationSnapshot>) -> Self {
        let snapshot = snapshot.into();
        let origin = match snapshot {
            DestinationSnapshot::Mac(_) => NativeOrigin::NativeMac,
            DestinationSnapshot::Windows(_) => NativeOrigin::NativeWindows,
        };
        Self { binding: Binding::new(request), context, snapshot, origin }
    }
    pub(super) fn check(&self, request: &RemoteUpdateRequest, node: &Node) -> Result<(), Refusal> {
        if Instant::now() >= self.binding.expires {
            return Err(Refusal::Expired);
        }
        if self.binding != Binding::new(request) || self.binding.node != node_bytes(node) {
            return Err(Refusal::InvalidRequest);
        }
        if !matches!(
            (&self.origin, &self.snapshot, request.target.node.os.as_str(), request.target.architecture, request.prepared.build_target()),
            (NativeOrigin::NativeMac, DestinationSnapshot::Mac(_), "macos", RemoteArchitecture::Arm64, "aarch64-apple-darwin")
                | (NativeOrigin::NativeWindows, DestinationSnapshot::Windows(_), "windows", RemoteArchitecture::X64, "x86_64-pc-windows-msvc")
        ) {
            return Err(Refusal::Target);
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
    digest(b"KTU2-place\0", &[observation.context.raw(), observation.snapshot.record().as_bytes()])
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

/// Closed platform selection. Raw wire data stays private to native transport.
#[derive(Debug)]
pub(super) enum DestinationSnapshot {
    Mac(MacSnapshot),
    Windows(WindowsSnapshot),
}
impl From<MacSnapshot> for DestinationSnapshot {
    fn from(s: MacSnapshot) -> Self {
        Self::Mac(s)
    }
}
impl DestinationSnapshot {
    pub(super) fn record(&self) -> &str {
        match self {
            Self::Mac(s) => &s.record,
            Self::Windows(s) => &s.record,
        }
    }
    pub(super) fn before(&self) -> BeforeImageFact {
        match self {
            Self::Mac(s) => s.before,
            Self::Windows(s) => s.before,
        }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn mac(self) -> MacSnapshot {
        match self {
            Self::Mac(s) => s,
            Self::Windows(_) => panic!("Mac fixture received a Windows observation"),
        }
    }
    pub(super) fn parse(target: &str, record: &str) -> Option<Self> {
        match target {
            "aarch64-apple-darwin" => MacSnapshot::parse(record).map(Self::Mac),
            "x86_64-pc-windows-msvc" => WindowsSnapshot::parse(record).map(Self::Windows),
            _ => None,
        }
    }
}
#[derive(Debug)]
pub(super) struct WindowsSnapshot {
    pub record: String,
    pub before: BeforeImageFact,
}
pub(super) fn unhex(text: &str, min: usize, max: usize) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !(min..=max).contains(&(text.len() / 2)) || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    text.as_bytes().as_chunks::<2>().0.iter().map(|b| u8::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok()).collect()
}
pub(super) fn sid_valid(sid: &[u8]) -> bool {
    (8..=68).contains(&sid.len()) && sid[0] == 1 && sid[1] <= 15 && sid.len() == 8 + 4 * sid[1] as usize
}
pub(super) fn profile_components(raw: &[u8]) -> Option<usize> {
    if !raw.len().is_multiple_of(2) || !(2..=256).contains(&raw.len()) {
        return None;
    }
    let units: Vec<_> = raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let profile = String::from_utf16(&units).ok()?;
    let b = profile.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_uppercase() || &b[1..3] != b":\\" {
        return None;
    }
    let components: Vec<_> = profile[3..].split('\\').collect();
    for c in &components {
        if c.is_empty()
            || *c == "."
            || *c == ".."
            || c.ends_with(['.', ' '])
            || c.encode_utf16().count() > 255
            || c.chars().any(|c| c.is_control() || "/:<>\"|?*".contains(c))
        {
            return None;
        }
    }
    Some(components.len())
}
impl WindowsSnapshot {
    pub(super) fn parse(record: &str) -> Option<Self> {
        if record.len() > 3072 || !record.is_ascii() {
            return None;
        }
        let f: Vec<_> = record.split('|').collect();
        let ["w1", sid, profile, bash, "windows", "x64", boot, tick, lease, parents, leaf] = f.as_slice() else {
            return None;
        };
        if !sid_valid(&unhex(sid, 8, 68)?) {
            return None;
        }
        let count = profile_components(&unhex(profile, 2, 256)?)?;
        let bash = unhex(bash, 1, 256)?;
        if std::str::from_utf8(&bash).ok()?.chars().any(char::is_control) {
            return None;
        }
        if unhex(boot, 16, 16)?.iter().all(|b| *b == 0) {
            return None;
        }
        let tick = decimal::<u64>(tick)?;
        let lease = decimal::<u64>(lease)?;
        if !(1..=120000).contains(&lease.checked_sub(tick)?) {
            return None;
        }
        let ps: Vec<_> = parents.split(';').collect();
        if ps.len() != count + 3 || ps.len() > 12 {
            return None;
        }
        for p in ps {
            let f: Vec<_> = p.split(',').collect();
            let [volume, id, attrs, security] = f.as_slice() else {
                return None;
            };
            decimal::<u64>(volume)?;
            unhex(id, 16, 16)?;
            decimal::<u32>(attrs)?;
            SamplerHash::parse(security)?;
        }
        let before = if *leaf == "M" {
            BeforeImageFact::Missing
        } else {
            let f: Vec<_> = leaf.split(',').collect();
            let ["P", volume, id, size, attrs, write, change, "1", security, hash] = f.as_slice() else {
                return None;
            };
            decimal::<u64>(volume)?;
            unhex(id, 16, 16)?;
            decimal::<u32>(attrs)?;
            decimal::<i64>(write)?;
            decimal::<i64>(change)?;
            SamplerHash::parse(security)?;
            if !(1..=super::protocol::SAMPLER_LIMIT as u64).contains(&decimal::<u64>(size)?) {
                return None;
            }
            BeforeImageFact::Present(SamplerHash::parse(hash)?)
        };
        Some(Self { record: record.into(), before })
    }
}
