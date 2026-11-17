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
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return None;
        }
        let mut bytes = [0; 32];
        for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            bytes[i] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Some(Self(bytes))
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
/// Bounded evidence only. A progress fact is not a present remote-state proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementFacts {
    pub token: AttemptToken,
    pub old_hash: Option<SamplerHash>,
    pub new_hash: SamplerHash,
    pub disposition: PlacementDisposition,
    pub retained_backup: bool,
    pub rollback_observed: bool,
}

/// Private trusted boundary; B/C must supply native-bound context and fixed glue.
/// Only test factories can currently construct an image/context in this module tree.
pub(super) struct Image<'a> {
    pub token: AttemptToken,
    pub version: &'a str,
    pub target: &'a str,
    pub manifest: &'a str,
    pub hash: &'a str,
    pub context: SamplerHash,
    pub glue: &'static [u8],
    pub bytes: &'a [u8],
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
        {
            return None;
        }
        let header = format!(
            "token={}\nversion={}\ntarget={}\nmanifest={}\nsize={}\nsha256={}\ncontext={}\n",
            self.token.as_str(),
            self.version,
            self.target,
            self.manifest,
            self.bytes.len(),
            self.hash,
            self.context.hex()
        );
        if header.len() > HEADER_LIMIT {
            return None;
        }
        let prefix = format!("KTU1 {} {} {}\n", header.len(), self.glue.len(), self.bytes.len());
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
}
impl Parser {
    pub(super) fn new(image: &Image<'_>) -> Self {
        Self {
            token: image.token,
            context: image.context,
            expected: SamplerHash::parse(image.hash).unwrap(),
            line: Vec::new(),
            total: 0,
            frames: 0,
            valid: true,
            terminal: false,
            stage: RemoteUpdateStage::Started,
            facts: None,
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
        let [protocol, token, context, event, new, old, backup, rollback] = fields.as_slice() else {
            return false;
        };
        if *protocol != "KTU1"
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
        let prior = self.facts;
        let disposition = match (*event, self.frames) {
            ("Started", 1) if old_hash.is_none() && !retained_backup && !rollback_observed => PlacementDisposition::Progress,
            ("Staged", 2) if old_hash.is_none() && !retained_backup && !rollback_observed => {
                self.stage = RemoteUpdateStage::Staged;
                PlacementDisposition::Progress
            }
            ("BackedUp", 3) if self.stage == RemoteUpdateStage::Staged && old_hash.is_some() && retained_backup && !rollback_observed => {
                self.stage = RemoteUpdateStage::BackedUp;
                PlacementDisposition::Progress
            }
            ("Placed", 4)
                if self.stage == RemoteUpdateStage::BackedUp && prior.is_some_and(|p| p.old_hash == old_hash) && retained_backup && !rollback_observed =>
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
                if prior.is_some()
                    && match self.stage {
                        RemoteUpdateStage::BackedUp => prior.is_some_and(|p| p.old_hash == old_hash) && (retained_backup || rollback_observed),
                        _ => old_hash.is_none() && !retained_backup && !rollback_observed,
                    } =>
            {
                self.terminal = true;
                PlacementDisposition::Failed
            }
            _ => return false,
        };
        self.facts = Some(PlacementFacts { token: self.token, old_hash, new_hash: self.expected, disposition, retained_backup, rollback_observed });
        true
    }
    pub(super) fn complete(&self) -> bool {
        self.valid && self.line.is_empty() && self.terminal
    }
    pub(super) fn confirmed(&self) -> bool {
        self.complete() && self.facts.is_some_and(|f| matches!(f.disposition, PlacementDisposition::Placed | PlacementDisposition::Unchanged))
    }
}
