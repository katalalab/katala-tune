//! Finite ASCII framing. This has no native destination authority or renderer.
use super::{RemoteUpdatePayload, RemoteUpdateStage};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const HEADER_LIMIT: usize = 16 * 1024;
pub(super) const GLUE_LIMIT: usize = 128 * 1024;
pub(super) const SAMPLER_LIMIT: usize = 256 * 1024 * 1024;
pub(super) const CHUNK: usize = 64 * 1024;
pub(super) const STDOUT_LIMIT: usize = 16 * 1024;
pub(super) const FRAME_LIMIT: usize = 4096;
pub(super) const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptToken {
    bytes: [u8; 64],
    len: u8,
}
impl AttemptToken {
    fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > 64 || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)) {
            return None;
        }
        let mut bytes = [0; 64];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Some(Self { bytes, len: text.len() as u8 })
    }
    pub(super) fn fresh(attempt: u64) -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        Self::parse(&format!("u{:x}-{:x}-{:x}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed), attempt)).unwrap()
    }
    pub fn as_str(&self) -> &str {
        // Construction admits ASCII only; this is not untrusted input.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap()
    }
}
/// A digest, never a remote path, account or raw observation string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SamplerHash([u8; 32]);
impl SamplerHash {
    pub(super) fn parse(text: &str) -> Option<Self> {
        if !crate::db::is_lower_hex(text, 64) {
            return None;
        }
        let mut bytes = [0; 32];
        for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            bytes[i] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Some(Self(bytes))
    }
    pub(super) fn raw(&self) -> &[u8] {
        &self.0
    }
    pub fn hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementDisposition {
    Progress,
    Placed,
    Unchanged,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeforeImageFact {
    Missing,
    Present(SamplerHash),
}
impl BeforeImageFact {
    pub fn hash(self) -> Option<SamplerHash> {
        match self {
            Self::Missing => None,
            Self::Present(h) => Some(h),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExchangeMode {
    Inspect,
    Place,
}
/// Bounded evidence only. A progress fact is not a present remote-state proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementFacts {
    pub token: AttemptToken,
    pub before: BeforeImageFact,
    pub new_hash: SamplerHash,
    pub disposition: PlacementDisposition,
    pub retained_backup: bool,
    pub rollback_observed: bool,
}

/// Fixed native or private synthetic image; no public wire construction.
pub(super) struct Image<'a> {
    pub token: AttemptToken,
    pub version: &'a str,
    pub target: &'a str,
    pub manifest: &'a str,
    pub hash: &'a str,
    pub context: SamplerHash,
    pub glue: &'static [u8],
    pub bytes: &'a [u8],
    pub mode: ExchangeMode,
    pub source_size: usize,
    pub source_hash: &'a str,
    pub ttl_ms: u64,
    pub observation: &'a str,
    pub before: BeforeImageFact,
}
impl<'a> Image<'a> {
    pub(super) fn payload(payload: &'a RemoteUpdatePayload<'a>, context: SamplerHash, glue: &'static [u8]) -> Self {
        Self {
            token: payload.attempt_token(),
            version: payload.version(),
            target: payload.build_target(),
            manifest: payload.manifest_sha256(),
            hash: payload.sha256(),
            context,
            glue,
            bytes: payload.bytes(),
            mode: ExchangeMode::Place,
            source_size: payload.bytes().len(),
            source_hash: payload.sha256(),
            ttl_ms: 1,
            observation: "-",
            before: payload.destination_observation().map_or(BeforeImageFact::Missing, |o| o.snapshot.before()),
        }
    }
    pub(super) fn header(&self) -> Option<(String, String)> {
        let field = |s: &str| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if self.version != crate::helpers::VERSION
            || !field(self.version)
            || !field(self.target)
            || SamplerHash::parse(self.manifest).is_none()
            || SamplerHash::parse(self.hash).is_none()
            || self.glue.len() > GLUE_LIMIT
            || self.bytes.len() > SAMPLER_LIMIT
            || self.source_size == 0
            || self.source_size > SAMPLER_LIMIT
            || SamplerHash::parse(self.source_hash).is_none()
            || !(1..=120000).contains(&self.ttl_ms)
            || self.observation.len() > 3072
            || !self.observation.is_ascii()
            || self.observation.contains(['\n', '\r'])
            || match self.mode {
                ExchangeMode::Inspect => !self.bytes.is_empty() || self.hash != crate::db::sha256_hex(b"") || self.observation != "-",
                ExchangeMode::Place => self.bytes.len() != self.source_size || self.hash != self.source_hash,
            }
        {
            return None;
        }
        let header = format!(
            "token={}\nversion={}\ntarget={}\nmanifest={}\nsize={}\nsha256={}\ncontext={}\nmode={}\nsource-size={}\nsource-sha256={}\nttl-ms={}\nobservation={}\n",
            self.token.as_str(),
            self.version,
            self.target,
            self.manifest,
            self.bytes.len(),
            self.hash,
            self.context.hex(),
            match self.mode {
                ExchangeMode::Inspect => "inspect",
                ExchangeMode::Place => "place",
            },
            self.source_size,
            self.source_hash,
            self.ttl_ms,
            self.observation
        );
        if header.len() > HEADER_LIMIT {
            return None;
        }
        let prefix = format!("KTU2 {} {} {}\n", header.len(), self.glue.len(), self.bytes.len());
        Some((prefix, header))
    }
}

pub(super) struct Parser {
    token: AttemptToken,
    context: SamplerHash,
    expected: SamplerHash,
    line: Vec<u8>,
    total: usize,
    frames: usize,
    valid: bool,
    terminal: bool,
    pub stage: RemoteUpdateStage,
    pub facts: Option<PlacementFacts>,
    mode: ExchangeMode,
    before: BeforeImageFact,
    pub observed: Option<super::observation::DestinationSnapshot>,
    target: String,
    pub inspect_failure: Option<super::macos::InspectFailure>,
}
impl Parser {
    pub(super) fn new(image: &Image<'_>) -> Self {
        Self {
            token: image.token,
            context: image.context,
            expected: SamplerHash::parse(image.source_hash).unwrap(),
            line: Vec::new(),
            total: 0,
            frames: 0,
            valid: true,
            terminal: false,
            stage: RemoteUpdateStage::Started,
            facts: None,
            mode: image.mode,
            target: image.target.into(),
            before: image.before,
            observed: None,
            inspect_failure: None,
        }
    }
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if !self.valid {
                return;
            }
            self.total += 1;
            if self.total > STDOUT_LIMIT || self.line.len() >= FRAME_LIMIT || self.terminal {
                self.valid = false;
                self.line.clear();
                return;
            }
            if byte == b'\n' {
                self.frames += 1;
                if self.frames > 8 || !self.frame() {
                    self.valid = false;
                }
                self.line.clear();
            } else {
                self.line.push(byte);
            }
        }
    }
    fn frame(&mut self) -> bool {
        let Ok(text) = std::str::from_utf8(&self.line) else {
            return false;
        };
        let fields: Vec<_> = text.split(' ').collect();
        if self.mode == ExchangeMode::Inspect {
            let ["KTU2", token, context, event, hash, detail] = fields.as_slice() else {
                return false;
            };
            if self.frames != 1
                || AttemptToken::parse(token) != Some(self.token)
                || SamplerHash::parse(context) != Some(self.context)
                || SamplerHash::parse(hash) != Some(self.expected)
            {
                return false;
            }
            match *event {
                "Observed" => {
                    let Some(snapshot) = super::observation::DestinationSnapshot::parse(&self.target, detail) else {
                        return false;
                    };
                    self.observed = Some(snapshot);
                }
                "InspectFailed" => {
                    let Some(reason) = super::macos::InspectFailure::parse(detail) else {
                        return false;
                    };
                    self.inspect_failure = Some(reason);
                }
                _ => return false,
            }
            self.terminal = true;
            return true;
        }
        let [protocol, token, context, event, new, old, backup, rollback] = fields.as_slice() else {
            return false;
        };
        if *protocol != "KTU2"
            || AttemptToken::parse(token) != Some(self.token)
            || SamplerHash::parse(context) != Some(self.context)
            || SamplerHash::parse(new) != Some(self.expected)
        {
            return false;
        }
        let old_hash = if *old == "-" {
            None
        } else {
            Some(match SamplerHash::parse(old) {
                Some(h) => h,
                None => return false,
            })
        };
        let (retained_backup, rollback_observed) = match (*backup, *rollback) {
            ("0", "0") => (false, false),
            ("1", "0") => (true, false),
            ("0", "1") => (false, true),
            _ => return false,
        };
        if old_hash != self.before.hash() {
            return false;
        }
        let present = matches!(self.before, BeforeImageFact::Present(_));
        let disposition = match (*event, self.frames) {
            ("Started", 1) if !retained_backup && !rollback_observed => PlacementDisposition::Progress,
            ("Staged", 2) if !retained_backup && !rollback_observed => {
                self.stage = RemoteUpdateStage::Staged;
                PlacementDisposition::Progress
            }
            ("BackedUp", 3) if present && self.stage == RemoteUpdateStage::Staged && retained_backup && !rollback_observed => {
                self.stage = RemoteUpdateStage::BackedUp;
                PlacementDisposition::Progress
            }
            ("Placed", n)
                if !rollback_observed
                    && if present {
                        n == 4 && self.stage == RemoteUpdateStage::BackedUp && retained_backup
                    } else {
                        n == 3 && self.stage == RemoteUpdateStage::Staged && !retained_backup
                    } =>
            {
                self.stage = RemoteUpdateStage::Placed;
                self.terminal = true;
                PlacementDisposition::Placed
            }
            ("Unchanged", 2) if old_hash == Some(self.expected) && !retained_backup && !rollback_observed => {
                self.stage = RemoteUpdateStage::Placed;
                self.terminal = true;
                PlacementDisposition::Unchanged
            }
            ("Failed", 2..=4)
                if self.facts.is_some()
                    && match self.stage {
                        RemoteUpdateStage::BackedUp => present,
                        RemoteUpdateStage::Staged if !present => !retained_backup,
                        _ => !retained_backup && !rollback_observed,
                    } =>
            {
                self.terminal = true;
                PlacementDisposition::Failed
            }
            _ => return false,
        };
        self.facts = Some(PlacementFacts { token: self.token, before: self.before, new_hash: self.expected, disposition, retained_backup, rollback_observed });
        true
    }
    pub(super) fn complete(&self) -> bool {
        self.valid && self.line.is_empty() && self.terminal
    }
    pub(super) fn confirmed(&self) -> bool {
        self.complete() && self.facts.is_some_and(|f| matches!(f.disposition, PlacementDisposition::Placed | PlacementDisposition::Unchanged))
    }
}
