//! Separate Docker Partial observations. No production admission issuer exists.
use crate::topology::{Edge, Entity};
use serde::{Deserialize, Serialize};

pub const CANDIDATE_TTL_MS: i64 = 86_400_000;
pub const CANDIDATE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const CANDIDATE_MAX_ENTITIES: usize = 4096;
pub const CANDIDATE_MAX_EDGES: usize = 16384;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerSemanticGap {
    UnobservedNetworkMembership,
    UnobservedVolumeTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerCandidatePayload {
    pub version: u32,
    pub scope_id: String,
    pub engine_identity: String,
    pub source_generation: String,
    pub settings_generation: String,
    pub facts_started_at_ms: i64,
    pub facts_observed_at_ms: i64,
    pub attempt_started_at_ms: i64,
    pub attempt_finished_at_ms: i64,
    pub gaps: Vec<DockerSemanticGap>,
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
}

/// DB reservation, not source or process-cleanup authority.
pub struct CandidateAttempt {
    id: String,
    ordinal: i64,
    holder: String,
    writer_generation: i64,
}

impl CandidateAttempt {
    pub(crate) fn reserved(id: String, ordinal: i64, holder: String, writer_generation: i64) -> Self {
        Self { id, ordinal, holder, writer_generation }
    }
    pub(crate) fn binding(&self) -> (&str, i64, &str, i64) {
        (&self.id, self.ordinal, &self.holder, self.writer_generation)
    }
}

/// Move-only exact payload authority. Only a cfg(test) synthetic issuer exists.
pub struct AdmittedDockerPartial {
    attempt: CandidateAttempt,
    payload: DockerCandidatePayload,
    digest: String,
}
impl AdmittedDockerPartial {
    pub(crate) fn parts(self) -> (CandidateAttempt, DockerCandidatePayload, String) {
        (self.attempt, self.payload, self.digest)
    }
    #[cfg(test)]
    pub(crate) fn synthetic(attempt: CandidateAttempt, payload: DockerCandidatePayload) -> Self {
        let digest = crate::db::sha256_hex(&serde_json::to_vec(&payload).unwrap());
        Self { attempt, payload, digest }
    }
    #[cfg(test)]
    pub(crate) fn tamper(&mut self, change: impl FnOnce(&mut DockerCandidatePayload)) {
        change(&mut self.payload);
    }
}

pub(crate) fn validate(payload: &DockerCandidatePayload, now: i64) -> Result<Vec<u8>, &'static str> {
    use crate::topology::{
        EntityKind, ScopeStatus,
        adapters::docker::{Container, DockerEngine, DockerPacket, Network, Volume, engine_snapshots},
    };
    fn digest(value: &str) -> bool {
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    if payload.version != 1
        || !digest(&payload.engine_identity)
        || !digest(&payload.source_generation)
        || !digest(&payload.settings_generation)
        || payload.scope_id.len() > 512
        || payload.scope_id.bytes().any(|b| b.is_ascii_control())
    {
        return Err("invalid candidate identity or version");
    }
    if !(0 <= payload.attempt_started_at_ms
        && payload.attempt_started_at_ms <= payload.facts_started_at_ms
        && payload.facts_started_at_ms <= payload.facts_observed_at_ms
        && payload.facts_observed_at_ms <= payload.attempt_finished_at_ms
        && payload.attempt_finished_at_ms <= now)
        || payload.facts_observed_at_ms.checked_add(CANDIDATE_TTL_MS).is_none_or(|expiry| now >= expiry)
    {
        return Err("invalid or expired candidate interval");
    }
    if payload.entities.len() > CANDIDATE_MAX_ENTITIES || payload.edges.len() > CANDIDATE_MAX_EDGES {
        return Err("candidate object budget exceeded");
    }
    let bytes = encode(payload)?;
    let engine = Entity::with_key(EntityKind::DockerEngine, "Docker engine", &payload.engine_identity);
    let suffix = format!(":engine:{}", engine.id.rsplit('.').next().unwrap_or_default());
    let base = payload.scope_id.strip_suffix(&suffix).filter(|base| base.ends_with(":docker")).ok_or("invalid candidate scope")?;
    let mut dto = DockerEngine { id: payload.engine_identity.clone(), containers: vec![], networks: vec![], volumes: vec![] };
    for entity in &payload.entities {
        match entity.kind {
            EntityKind::DockerContainer => {
                dto.containers.push(serde_json::from_value::<Container>(entity.attributes.clone()).map_err(|_| "invalid container")?)
            }
            EntityKind::DockerNetwork => dto.networks.push(serde_json::from_value::<Network>(entity.attributes.clone()).map_err(|_| "invalid network")?),
            EntityKind::DockerVolume => dto.volumes.push(serde_json::from_value::<Volume>(entity.attributes.clone()).map_err(|_| "invalid volume")?),
            EntityKind::DockerEngine | EntityKind::DockerMount | EntityKind::Host | EntityKind::ExecutionEnvironment => {}
            _ => return Err("non-Docker candidate kind"),
        }
    }
    let networks = dto.networks.iter().map(|network| network.id.as_str()).collect::<std::collections::HashSet<_>>();
    let volumes = dto.volumes.iter().map(|volume| volume.id.as_str()).collect::<std::collections::HashSet<_>>();
    let missing_network = dto.containers.iter().any(|container| container.networks.iter().any(|id| !networks.contains(id.as_str())));
    let missing_volume = dto
        .containers
        .iter()
        .any(|container| container.mounts.iter().any(|mount| mount.kind == "volume" && !mount.volume_hash.as_deref().is_some_and(|id| volumes.contains(id))));
    let mut gaps = vec![];
    if missing_network {
        gaps.push(DockerSemanticGap::UnobservedNetworkMembership);
    }
    if missing_volume {
        gaps.push(DockerSemanticGap::UnobservedVolumeTarget);
    }
    if gaps.is_empty() || gaps != payload.gaps {
        return Err("candidate lacks exact semantic gaps");
    }
    let snapshot = DockerPacket { engines: vec![dto] }.snapshot(base.into(), payload.facts_observed_at_ms).map_err(|_| "invalid candidate DTO")?;
    let mut projected = engine_snapshots(snapshot).into_iter().nth(1).ok_or("missing candidate engine")?;
    if projected.status != ScopeStatus::Partial {
        return Err("candidate is not semantic Partial");
    }
    let mut entities = payload.entities.clone();
    let mut edges = payload.edges.clone();
    entities.sort_by(|a, b| a.id.cmp(&b.id));
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    if entities.windows(2).any(|e| e[0].id == e[1].id) || edges.windows(2).any(|e| e[0].id == e[1].id) {
        return Err("duplicate candidate identity");
    }
    projected.entities.sort_by(|a, b| a.id.cmp(&b.id));
    projected.edges.sort_by(|a, b| a.id.cmp(&b.id));
    if entities != projected.entities || edges != projected.edges {
        return Err("candidate differs from sanitized projection");
    }
    Ok(bytes)
}

pub(crate) fn encode(value: &impl Serialize) -> Result<Vec<u8>, &'static str> {
    struct LimitedJson(Vec<u8>);
    impl std::io::Write for LimitedJson {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > CANDIDATE_MAX_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("candidate byte budget exceeded"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = LimitedJson(Vec::new());
    serde_json::to_writer(&mut encoded, value).map_err(|_| "candidate byte budget exceeded")?;
    Ok(encoded.0)
}
