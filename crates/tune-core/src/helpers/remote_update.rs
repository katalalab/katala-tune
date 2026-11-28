//! One explicitly selected remote placement, owned by the controlled monitor.
//! Native transport remains opt-in; the GPUI default has no update backend.
use super::native_install::PreparedNativeSampler;
use crate::{
    engine::Engine,
    live::Live,
    monitor::{self, RunMode, RunRequest},
    nodes::{Config, Node},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteArchitecture {
    Arm64,
    X64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    InvalidRequest,
    Source,
    Target,
    Inputs,
    Policy,
    Session,
    Expired,
    Withdrawn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Busy,
    Closed,
    Refused(Refusal),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteUpdateStage {
    Started,
    Staged,
    BackedUp,
    Placed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncertainty {
    ResponseLost,
    Exit,
    ResponseMismatch,
    Panic,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteUpdateOutcome {
    Refused {
        attempt_id: u64,
        reason: Refusal,
    },
    /// Expected bytes were placed. The running sampler version is unconfirmed.
    ConfirmedPlacement {
        attempt_id: u64,
    },
    Unknown {
        attempt_id: u64,
        stage: RemoteUpdateStage,
        cause: Uncertainty,
    },
}
impl RemoteUpdateOutcome {
    pub fn placement_known(self) -> bool {
        !matches!(self, Self::Unknown { .. })
    }
}
/// Entire checked Node, not a list or a last-good cache. Architecture is an
/// explicit operator declaration; it is not hardware attestation.
#[derive(Debug)]
pub struct RemoteTargetSnapshot {
    node: Node,
    architecture: RemoteArchitecture,
}
impl RemoteTargetSnapshot {
    pub fn checked(engine: &Engine, id: &str, architecture: Option<RemoteArchitecture>) -> Result<Self, Refusal> {
        let (cfg, _) = monitor::controlled_inputs(engine).map_err(|_| Refusal::Inputs)?;
        let node = unique(&cfg, id)?;
        if node.local || !node.is_active() || !super::same_os(node) {
            return Err(Refusal::Target);
        }
        Ok(Self { node: node.clone(), architecture: architecture.ok_or(Refusal::Target)? })
    }
    fn matches(&self, cfg: &Config) -> bool {
        unique(cfg, &self.node.id).is_ok_and(|n| {
            n.id == self.node.id
                && n.alias == self.node.alias
                && n.os == self.node.os
                && n.local == self.node.local
                && n.shared == self.node.shared
                && n.raw == self.node.raw
        })
    }
}
fn unique<'a>(cfg: &'a Config, id: &str) -> Result<&'a Node, Refusal> {
    let mut matching = cfg.nodes.iter().filter(|n| n.id == id);
    let node = matching.next().ok_or(Refusal::Target)?;
    if matching.next().is_some() {
        return Err(Refusal::Target);
    }
    Ok(node)
}
/// Move-only authority: one attempt, Start, monotonic deadline, Node and source.
#[derive(Debug)]
pub struct RemoteUpdateRequest {
    attempt_id: u64,
    token: AttemptToken,
    start_seq: u64,
    expires: Instant,
    target: RemoteTargetSnapshot,
    prepared: PreparedNativeSampler,
    observation: Option<RemoteDestinationObservation>,
}
impl RemoteUpdateRequest {
    pub fn new(attempt_id: u64, start_seq: u64, expires: Instant, target: RemoteTargetSnapshot, prepared: PreparedNativeSampler) -> Result<Self, Refusal> {
        let remaining = expires.checked_duration_since(Instant::now()).ok_or(Refusal::Expired)?;
        if attempt_id == 0 || remaining > Duration::from_secs(120) {
            return Err(Refusal::InvalidRequest);
        }
        let expected = match (target.node.os.as_str(), target.architecture) {
            ("macos", RemoteArchitecture::Arm64) => "aarch64-apple-darwin",
            ("windows", RemoteArchitecture::X64) => "x86_64-pc-windows-msvc",
            _ => return Err(Refusal::Target),
        };
        if prepared.version() != super::VERSION || prepared.build_target() != expected || !super::binary::declared_target(prepared.remote_bytes(), expected) {
            return Err(Refusal::Source);
        }
        Ok(Self { attempt_id, token: AttemptToken::fresh(attempt_id), start_seq, expires, target, prepared, observation: None })
    }
    pub fn with_observation(mut self, observation: RemoteDestinationObservation) -> Result<Self, Refusal> {
        if self.observation.is_some() {
            return Err(Refusal::InvalidRequest);
        }
        observation.check(&self, &self.target.node)?;
        self.observation = Some(observation);
        Ok(self)
    }
    fn payload(&self) -> RemoteUpdatePayload<'_> {
        RemoteUpdatePayload { request: self }
    }
    fn validate(&self, engine: &Engine, live: &Live) -> Result<(), Refusal> {
        let (cfg, settings) = monitor::controlled_inputs(engine).map_err(|_| Refusal::Inputs)?;
        if !self.target.matches(&cfg) {
            return Err(Refusal::Target);
        }
        if !monitor::controlled_collect(engine, &settings, crate::db::now_ms()).map_err(|_| Refusal::Inputs)? {
            return Err(Refusal::Policy);
        }
        if !super::agent_outdated(&self.target.node, live.brief(&self.target.node.id).as_ref()) {
            return Err(Refusal::Target);
        }
        Ok(())
    }
    fn current(&self, request: &RunRequest) -> Result<(), Refusal> {
        if request.mode != RunMode::Run || request.start_seq != self.start_seq {
            return Err(Refusal::Session);
        }
        if Instant::now() >= self.expires {
            return Err(Refusal::Expired);
        }
        Ok(())
    }
}
/// Borrowed immutable prepared bytes. No caller can construct an unvalidated payload.
pub struct RemoteUpdatePayload<'a> {
    request: &'a RemoteUpdateRequest,
}
impl RemoteUpdatePayload<'_> {
    pub fn attempt_token(&self) -> AttemptToken {
        self.request.token
    }
    pub fn destination_observation(&self) -> Option<&RemoteDestinationObservation> {
        self.request.observation.as_ref()
    }
    pub fn version(&self) -> &str {
        self.request.prepared.version()
    }
    pub fn build_target(&self) -> &str {
        self.request.prepared.build_target()
    }
    pub fn manifest_sha256(&self) -> &str {
        self.request.prepared.manifest_sha256()
    }
    pub fn bytes(&self) -> &[u8] {
        self.request.prepared.remote_bytes()
    }
    pub fn sha256(&self) -> &str {
        self.request.prepared.sampler_sha256()
    }
    pub fn expected_response(&self) -> String {
        format!("placed-{}", self.sha256())
    }
}
/// Bounded observation; raw stdout/stderr is never retained in control state.
pub struct RemoteUpdateReply {
    code: Option<i32>,
    hash: Option<String>,
    stage: RemoteUpdateStage,
    response_complete: bool,
    local_settled: bool,
    facts: Option<PlacementFacts>,
    before_start_refusal: Option<Refusal>,
}
impl RemoteUpdateReply {
    /// Synthetic/third-party implementations assert all local owned work is
    /// settled when returning this observation, even if the response was lost.
    /// An exit code and matching marker from an incomplete stream stay Unknown.
    /// OwnedSshUpdateTransport constructs its reply only after join/wait/EOF.
    pub fn observed(code: Option<i32>, response: &[u8], stage: RemoteUpdateStage, response_complete: bool) -> Self {
        let hash = response
            .strip_prefix(b"placed-")
            .filter(|s| s.len() == 64 && s.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))
            .and_then(|s| std::str::from_utf8(s).ok())
            .map(str::to_owned);
        Self { code, hash, stage, response_complete, local_settled: true, facts: None, before_start_refusal: None }
    }
}
pub type RemoteUpdateFuture<'a> = Pin<Box<dyn Future<Output = RemoteUpdateReply> + Send + 'a>>;
/// Implementations must own their work through completion. The native backend
/// is opt-in; the legacy killing System runner is excluded.
pub trait RemoteUpdateTransport: Send + Sync {
    fn preflight(&self, _node: &Node, _payload: RemoteUpdatePayload<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn place<'a>(&'a self, node: &'a Node, payload: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a>;
}
/// Non-cloneable checked admission, held only across the short host/Gate boundary.
pub struct CheckedRemoteUpdate(RemoteUpdateRequest);

enum Slot {
    Idle,
    Pending(Box<RemoteUpdateRequest>),
    Claimed(u64),
    Finished(RemoteUpdateOutcome),
}
struct State {
    slot: Slot,
    closed: bool,
    last_attempt: u64,
    completion: Option<bool>,
    facts: Option<PlacementFacts>,
}
pub struct RemoteUpdateControl {
    state: Mutex<State>,
    transport: Arc<dyn RemoteUpdateTransport>,
}
impl RemoteUpdateControl {
    pub fn new(transport: Arc<dyn RemoteUpdateTransport>) -> Self {
        Self { state: Mutex::new(State { slot: Slot::Idle, closed: false, last_attempt: 0, completion: None, facts: None }), transport }
    }
    fn available(&self) -> Result<(), AdmissionError> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(AdmissionError::Closed);
        }
        if matches!(state.slot, Slot::Pending(_) | Slot::Claimed(_)) {
            return Err(AdmissionError::Busy);
        }
        Ok(())
    }
    /// Performs all checked ledger/settings IO before host/Gate/watch locks.
    pub fn check_admission(&self, engine: &Engine, live: &Live, update: RemoteUpdateRequest) -> Result<CheckedRemoteUpdate, AdmissionError> {
        self.available()?;
        update.validate(engine, live).map_err(AdmissionError::Refused)?;
        self.transport.preflight(&update.target.node, update.payload()).map_err(AdmissionError::Refused)?;
        Ok(CheckedRemoteUpdate(update))
    }
    /// Memory-only registration. Core rechecks current inputs before claim;
    /// this opaque value cannot manufacture a source or skip checked admission.
    pub fn admit_checked(&self, request: &watch::Sender<RunRequest>, checked: CheckedRemoteUpdate) -> Result<(), AdmissionError> {
        let update = checked.0;
        let current = request.borrow();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(AdmissionError::Closed);
        }
        if matches!(state.slot, Slot::Pending(_) | Slot::Claimed(_)) {
            return Err(AdmissionError::Busy);
        }
        update.current(&current).map_err(AdmissionError::Refused)?;
        if update.attempt_id <= state.last_attempt {
            return Err(AdmissionError::Refused(Refusal::InvalidRequest));
        }
        state.last_attempt = update.attempt_id;
        state.completion = None;
        state.facts = None;
        state.slot = Slot::Pending(Box::new(update));
        drop(state);
        drop(current);
        request.send_modify(|_| {});
        Ok(())
    }
    pub fn admit(&self, engine: &Engine, live: &Live, request: &watch::Sender<RunRequest>, update: RemoteUpdateRequest) -> Result<(), AdmissionError> {
        self.admit_checked(request, self.check_admission(engine, live, update)?)
    }
    /// A stale window cannot withdraw a later attempt. Claimed work is irrevocable.
    pub fn withdraw(&self, attempt_id: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(&state.slot, Slot::Pending(r) if r.attempt_id == attempt_id) {
            state.slot = Slot::Finished(RemoteUpdateOutcome::Refused { attempt_id, reason: Refusal::Withdrawn });
            true
        } else {
            false
        }
    }
    pub fn outcome(&self) -> Option<RemoteUpdateOutcome> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Slot::Finished(outcome) = state.slot { Some(outcome) } else { None }
    }
    /// Placement certainty, local work settlement and journal ACK are independent.
    pub fn local_completion(&self) -> Option<bool> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.slot {
            Slot::Finished(RemoteUpdateOutcome::Refused { .. }) => Some(true),
            Slot::Finished(_) => state.completion,
            _ => None,
        }
    }
    pub fn placement_facts(&self) -> Option<PlacementFacts> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).facts
    }
    pub fn busy(&self) -> bool {
        matches!(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).slot, Slot::Pending(_) | Slot::Claimed(_))
    }
    pub fn refuse_pending(&self, reason: Refusal) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Slot::Pending(r) = &state.slot {
            state.slot = Slot::Finished(RemoteUpdateOutcome::Refused { attempt_id: r.attempt_id, reason });
        }
    }
    /// Observe the latest watch value and retire invalid Pending work under the
    /// same short watch -> slot boundary, including coalesced Start/stop changes.
    /// A valid newer request and already Claimed work are preserved.
    pub(crate) fn observe_request(&self, request: &mut watch::Receiver<RunRequest>) -> RunRequest {
        let current = request.borrow_and_update();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Slot::Pending(update) = &state.slot
            && let Err(reason) = update.current(&current)
        {
            state.slot = Slot::Finished(RemoteUpdateOutcome::Refused { attempt_id: update.attempt_id, reason });
        }
        *current
    }
    pub(crate) fn refuse_start(&self, start_seq: u64, reason: Refusal) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Slot::Pending(r) = &state.slot
            && r.start_seq == start_seq
        {
            state.slot = Slot::Finished(RemoteUpdateOutcome::Refused { attempt_id: r.attempt_id, reason });
        }
    }
    /// Validation IO precedes the watch guard. Claim, Pause/Exit/Start and
    /// withdrawal linearize at watch -> slot with no guard across transport IO.
    pub(crate) fn claim(&self, engine: &Engine, live: &Live, request: &watch::Sender<RunRequest>) -> Option<RemoteUpdateRequest> {
        // Temporarily borrow only to inspect. Never hold the slot lock for IO.
        let id = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &state.slot {
                Slot::Pending(r) => r.attempt_id,
                _ => return None,
            }
        };
        // Move the request out only at the synchronous claim boundary below.
        // A checked full ledger and policy are reloaded independently of cache.
        let inputs = monitor::controlled_inputs(engine);
        let policy = inputs.as_ref().ok().and_then(|(_, s)| monitor::controlled_collect(engine, s, crate::db::now_ms()).ok());
        let current = request.borrow();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Slot::Pending(r) = &state.slot else { return None };
        if r.attempt_id != id {
            return None;
        }
        let valid = r.current(&current).and_then(|_| {
            let (cfg, _) = inputs.as_ref().map_err(|_| Refusal::Inputs)?;
            if !r.target.matches(cfg) {
                return Err(Refusal::Target);
            }
            if policy != Some(true) {
                return Err(Refusal::Policy);
            }
            if !super::agent_outdated(&r.target.node, live.brief(&r.target.node.id).as_ref()) {
                return Err(Refusal::Target);
            }
            self.transport.preflight(&r.target.node, r.payload())
        });
        if let Err(reason) = valid {
            state.slot = Slot::Finished(RemoteUpdateOutcome::Refused { attempt_id: id, reason });
            return None;
        }
        let Slot::Pending(update) = std::mem::replace(&mut state.slot, Slot::Claimed(id)) else { unreachable!() };
        Some(*update)
    }
    pub(crate) async fn execute(&self, update: RemoteUpdateRequest) {
        let attempt_id = update.attempt_id;
        let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.transport.place(&update.target.node, update.payload())));
        let observed = match future {
            Ok(mut future) => {
                std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                    Ok(std::task::Poll::Ready(reply)) => std::task::Poll::Ready(Ok(reply)),
                    Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
                    Err(_) => std::task::Poll::Ready(Err(())),
                })
                .await
            }
            Err(_) => Err(()),
        };
        let local_settled = observed.as_ref().is_ok_and(|reply| reply.local_settled);
        let facts = observed.as_ref().ok().and_then(|reply| reply.facts);
        let outcome = match observed {
            Ok(reply) if reply.before_start_refusal.is_some() => RemoteUpdateOutcome::Refused { attempt_id, reason: reply.before_start_refusal.unwrap() },
            Ok(reply)
                if reply.local_settled
                    && reply.response_complete
                    && reply.stage == RemoteUpdateStage::Placed
                    && reply.code == Some(0)
                    && reply.hash.as_deref() == Some(update.prepared.sampler_sha256()) =>
            {
                RemoteUpdateOutcome::ConfirmedPlacement { attempt_id }
            }
            Ok(reply) => RemoteUpdateOutcome::Unknown {
                attempt_id,
                stage: reply.stage,
                cause: if !reply.response_complete || reply.code.is_none() {
                    Uncertainty::ResponseLost
                } else if reply.code != Some(0) {
                    Uncertainty::Exit
                } else {
                    Uncertainty::ResponseMismatch
                },
            },
            Err(()) => RemoteUpdateOutcome::Unknown { attempt_id, stage: RemoteUpdateStage::Started, cause: Uncertainty::Panic },
        };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(matches!(state.slot, Slot::Claimed(id) if id == attempt_id));
        state.completion = Some(local_settled);
        state.facts = facts;
        state.closed |= !outcome.placement_known();
        state.slot = Slot::Finished(outcome);
    }
}

mod macos;
mod observation;
mod windows;
#[cfg(test)]
mod windows_tests;
pub use macos::{InspectFailure, RemoteInspectError};
pub use observation::RemoteDestinationObservation;
mod protocol;
mod ssh;
pub use protocol::{AttemptToken, BeforeImageFact, PlacementDisposition, PlacementFacts, SamplerHash};
pub use ssh::OwnedSshUpdateTransport;
#[cfg(test)]
mod ssh_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod macos_tests;
