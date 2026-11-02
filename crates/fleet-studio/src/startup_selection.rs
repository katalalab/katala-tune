//! Explicit, session-only selection of saved machine summaries at startup.
use std::ffi::OsString;
use std::time::{Duration, Instant};

const PREFIX: &str = "--saved-summary=";
const MAX_IDS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupRequest {
    None,
    Requested(Vec<String>),
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    InvalidArgs,
    Unavailable,
    ConflictingGrant,
    GrantFailed,
    Expired,
}

pub struct Candidate<'a> {
    pub id: &'a str,
    pub fingerprint: Option<&'a str>,
}

/// The operator's startup choice is used at most once, including on failure.
pub struct PendingSelection {
    request: Option<StartupRequest>,
    deadline: Option<Instant>,
}

/// Sample mode never turns real machine IDs into read grants.
pub fn initialize(sample: bool, request: StartupRequest) -> (PendingSelection, Option<&'static str>) {
    initialize_at(sample, request, Instant::now())
}

pub fn initialize_at(sample: bool, request: StartupRequest, process_start: Instant) -> (PendingSelection, Option<&'static str>) {
    let (request, notice) = match (sample, request) {
        (_, StartupRequest::Invalid) => (StartupRequest::None, Some("起動時の機体指定が不正です。保存要約は自動表示しません。")),
        (true, StartupRequest::Requested(_)) => (StartupRequest::None, Some("サンプルでは起動時の機体指定を使いません。")),
        (_, request) => (request, None),
    };
    (PendingSelection::new_at(request, process_start), notice)
}

impl PendingSelection {
    pub fn new(request: StartupRequest) -> Self {
        Self::new_at(request, Instant::now())
    }

    pub fn new_at(request: StartupRequest, now: Instant) -> Self {
        Self { request: if request == StartupRequest::None { None } else { Some(request) }, deadline: now.checked_add(Duration::from_secs(5)) }
    }

    pub fn cancel(&mut self) -> bool {
        self.request.take().is_some()
    }

    pub fn consume(&mut self, cards: &[Candidate<'_>]) -> Result<Vec<(String, String)>, PlanError> {
        self.consume_at(cards, Instant::now())
    }

    pub fn consume_at(&mut self, cards: &[Candidate<'_>], now: Instant) -> Result<Vec<(String, String)>, PlanError> {
        let Some(request) = self.request.take() else { return Ok(Vec::new()) };
        if self.deadline.is_none_or(|deadline| now >= deadline) {
            return Err(PlanError::Expired);
        }
        plan(&request, cards)
    }

    /// All requested targets must resolve before any new session grant is made.
    pub fn apply(&mut self, cards: &[Candidate<'_>], grants: &mut crate::scope::Grants) -> Result<usize, PlanError> {
        self.apply_with_clock(cards, grants, Instant::now)
    }

    pub fn apply_at(&mut self, cards: &[Candidate<'_>], grants: &mut crate::scope::Grants, now: Instant) -> Result<usize, PlanError> {
        self.apply_with_clock(cards, grants, || now)
    }

    pub fn apply_with_clock(
        &mut self,
        cards: &[Candidate<'_>],
        grants: &mut crate::scope::Grants,
        mut clock: impl FnMut() -> Instant,
    ) -> Result<usize, PlanError> {
        let selected = self.consume_at(cards, clock())?;
        if selected.is_empty() {
            return Ok(0);
        }
        if selected.iter().any(|(id, fingerprint)| grants.has_active_id(id) && grants.matching(id, fingerprint).is_none()) {
            return Err(PlanError::ConflictingGrant);
        }
        let mut created: Vec<String> = Vec::new();
        for (id, fingerprint) in selected {
            if self.deadline.is_none_or(|deadline| clock() >= deadline) {
                for prior in created {
                    grants.revoke(&prior);
                }
                return Err(PlanError::Expired);
            }
            if grants.matching(&id, &fingerprint).is_some() {
                continue;
            }
            if !grants.authorize(&id, &fingerprint) {
                for prior in created {
                    grants.revoke(&prior);
                }
                return Err(PlanError::GrantFailed);
            }
            created.push(id);
        }
        if self.deadline.is_none_or(|deadline| clock() >= deadline) {
            for prior in created {
                grants.revoke(&prior);
            }
            return Err(PlanError::Expired);
        }
        Ok(created.len())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.trim() == id && !id.chars().any(char::is_control)
}

/// Each option is one explicit machine ID. Invalid arguments never select a subset.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> StartupRequest {
    let mut ids = Vec::new();
    for arg in args {
        let Some(arg) = arg.to_str() else { return StartupRequest::Invalid };
        if arg == "--saved-summary" {
            return StartupRequest::Invalid;
        }
        let Some(id) = arg.strip_prefix(PREFIX) else { continue };
        if !valid_id(id) {
            return StartupRequest::Invalid;
        }
        if !ids.iter().any(|known| known == id) {
            if ids.len() == MAX_IDS {
                return StartupRequest::Invalid;
            }
            ids.push(id.to_owned());
        }
    }
    if ids.is_empty() { StartupRequest::None } else { StartupRequest::Requested(ids) }
}

/// Resolve only exact, unique current ledger IDs with usable fingerprints.
pub fn plan(request: &StartupRequest, cards: &[Candidate<'_>]) -> Result<Vec<(String, String)>, PlanError> {
    let ids = match request {
        StartupRequest::None => return Ok(Vec::new()),
        StartupRequest::Invalid => return Err(PlanError::InvalidArgs),
        StartupRequest::Requested(ids) => ids,
    };
    if ids.is_empty() || ids.len() > MAX_IDS || ids.iter().any(|id| !valid_id(id)) {
        return Err(PlanError::InvalidArgs);
    }
    let mut selected = Vec::with_capacity(ids.len());
    for id in ids {
        if selected.iter().any(|(known, _)| known == id) {
            return Err(PlanError::InvalidArgs);
        }
        let mut matches = cards.iter().filter(|card| card.id == id);
        let card = matches.next().ok_or(PlanError::Unavailable)?;
        if matches.next().is_some() {
            return Err(PlanError::Unavailable);
        }
        let fingerprint = card.fingerprint.filter(|fingerprint| !fingerprint.is_empty()).ok_or(PlanError::Unavailable)?;
        selected.push((id.clone(), fingerprint.to_owned()));
    }
    Ok(selected)
}
