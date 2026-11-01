use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;

pub const MODEL_MAX_AGE_MS: i64 = 5 * 60 * 1_000;
const PREPARED: u8 = 0;
const DISPATCHED: u8 = 1;
const RUNNING: u8 = 2;
const CANCELLED_PREPARED: u8 = 3;
const CANCELLED_DISPATCHED: u8 = 4;
const CANCELLED_RUNNING: u8 = 5;
const FINISHED: u8 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    OpenAi,
    Ollama,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    Unavailable,
    TimedOut,
    InvalidResponse,
    Http(u16),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelObservation {
    pub latest: Option<Result<Vec<String>, ModelError>>,
    pub checked_at: Option<i64>,
    pub last_good: Option<(Vec<String>, i64)>,
}

impl ModelObservation {
    pub fn begin_refresh(&mut self) {
        self.latest = None;
        self.checked_at = None;
    }

    pub fn last_good_freshness(&self, now: i64) -> crate::Observation {
        crate::observation(self.last_good.as_ref().map(|(_, at)| *at), false, true, now, MODEL_MAX_AGE_MS)
    }

    pub fn record(&mut self, result: Result<Vec<String>, ModelError>, at: i64) {
        if let Ok(models) = &result {
            self.last_good = Some((models.clone(), at));
        }
        self.latest = Some(result);
        self.checked_at = Some(at);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    pub label: String,
    pub address: SocketAddr,
    pub protocol: Protocol,
}

/// Return only endpoints that may be queried without an additional session grant.
pub fn configured(value: Option<&str>) -> (Vec<Endpoint>, usize) {
    let (endpoints, rejected) = candidates(value);
    (endpoints.into_iter().filter(|endpoint| endpoint.address.ip().is_loopback()).collect(), rejected)
}

fn candidates(value: Option<&str>) -> (Vec<Endpoint>, usize) {
    let mut endpoints: Vec<_> = ["Strata (local)|127.0.0.1:18080|openai", "Ollama (local)|127.0.0.1:11434|ollama", "LM Studio (local)|127.0.0.1:1234|openai"]
        .into_iter()
        .filter_map(endpoint)
        .collect();
    let mut rejected = 0;
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        for (index, item) in value.split(';').enumerate() {
            if index >= 8 {
                rejected += 1;
                continue;
            }
            if let Some(item) = endpoint(item.trim()) {
                endpoints.push(item);
            } else {
                rejected += 1;
            }
        }
    }
    (endpoints, rejected)
}

/// A configured endpoint and its observation for the current application session.
pub struct ModelEntry {
    pub endpoint: Endpoint,
    pub authorized: bool,
    pub observation: ModelObservation,
    grant: Option<ModelGrant>,
    pending: Option<Arc<AtomicU8>>,
}

impl ModelEntry {
    pub fn grant(&self) -> Option<&ModelGrant> {
        self.grant.as_ref()
    }

    pub fn querying(&self) -> bool {
        self.pending.as_ref().is_some_and(|permit| matches!(permit.load(Ordering::Acquire), DISPATCHED | RUNNING))
    }

    fn invalidate(&mut self) -> Option<(ModelRevocation, bool)> {
        let permit = self.pending.take()?;
        loop {
            let old = permit.load(Ordering::Acquire);
            let (new, outcome, await_completion) = match old {
                PREPARED => (CANCELLED_PREPARED, ModelRevocation::Prevented, false),
                DISPATCHED => (CANCELLED_DISPATCHED, ModelRevocation::Prevented, true),
                RUNNING => (CANCELLED_RUNNING, ModelRevocation::InProgress, true),
                CANCELLED_PREPARED => return Some((ModelRevocation::Prevented, false)),
                CANCELLED_DISPATCHED => return Some((ModelRevocation::Prevented, true)),
                CANCELLED_RUNNING => return Some((ModelRevocation::InProgress, true)),
                _ => return Some((ModelRevocation::Idle, false)),
            };
            if permit.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Some((outcome, await_completion));
            }
        }
    }

    fn request(&mut self) -> ModelQuery {
        let _ = self.invalidate();
        self.observation.begin_refresh();
        let permit = Arc::new(AtomicU8::new(PREPARED));
        self.pending = Some(permit.clone());
        ModelQuery { endpoint: self.endpoint.clone(), permit }
    }
}

/// An immutable, one-request permission. Revocation invalidates queued work.
#[derive(Clone)]
pub struct ModelQuery {
    endpoint: Endpoint,
    permit: Arc<AtomicU8>,
}

impl ModelQuery {
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn active(&self) -> bool {
        matches!(self.permit.load(Ordering::Acquire), PREPARED | DISPATCHED | RUNNING)
    }

    /// Once the callback begins, a later revocation cannot undo its I/O.
    pub fn start<T>(&self, operation: impl FnOnce(&Endpoint) -> T) -> Option<T> {
        self.permit.compare_exchange(DISPATCHED, RUNNING, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| operation(&self.endpoint))
    }
}

/// A confirmation can only authorize the exact configuration that was displayed.
#[derive(Clone, Debug)]
pub struct ModelConfirmation {
    endpoint: Endpoint,
    nonce: Arc<()>,
}

impl PartialEq for ModelConfirmation {
    fn eq(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint && Arc::ptr_eq(&self.nonce, &other.nonce)
    }
}
impl Eq for ModelConfirmation {}

impl ModelConfirmation {
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn description(&self) -> String {
        format!("{} / {} / 平文 HTTP / {}", self.endpoint.address, self.endpoint.protocol.label(), self.endpoint.label)
    }
}

#[derive(Clone, Debug)]
pub struct ModelGrant {
    endpoint: Endpoint,
    nonce: Arc<()>,
}

impl PartialEq for ModelGrant {
    fn eq(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint && Arc::ptr_eq(&self.nonce, &other.nonce)
    }
}
impl Eq for ModelGrant {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRevocation {
    Prevented,
    InProgress,
    Idle,
}

/// Coalesce refreshes while one or more loads are still active.
#[derive(Default)]
pub struct RefreshQueue {
    pending: bool,
}

impl RefreshQueue {
    pub fn request(&mut self, busy: bool) -> bool {
        if busy {
            self.pending = true;
            false
        } else {
            self.pending = false;
            true
        }
    }

    pub fn ready(&mut self, busy: bool) -> bool {
        if !busy && self.pending {
            self.pending = false;
            true
        } else {
            false
        }
    }

    pub fn pending(&self) -> bool {
        self.pending
    }
}

/// Tracks explicit remote grants and rejects responses from earlier requests.
#[derive(Default)]
pub struct ModelScope {
    entries: Vec<ModelEntry>,
    cancelled: Vec<ModelQuery>,
    rejected: usize,
    duplicates: usize,
    confirmation: Option<ModelConfirmation>,
    confirmations_suspended: bool,
}

impl ModelScope {
    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    pub fn rejected(&self) -> usize {
        self.rejected
    }

    pub fn duplicates(&self) -> usize {
        self.duplicates
    }

    pub fn querying(&self) -> bool {
        self.entries.iter().any(ModelEntry::querying) || !self.cancelled.is_empty()
    }

    pub fn pending_cancellations(&self) -> usize {
        self.cancelled.len()
    }

    pub fn cancellation(&self, query: &ModelQuery) -> Option<ModelRevocation> {
        let item = self.cancelled.iter().find(|item| Arc::ptr_eq(&item.permit, &query.permit))?;
        match item.permit.load(Ordering::Acquire) {
            CANCELLED_DISPATCHED => Some(ModelRevocation::Prevented),
            CANCELLED_RUNNING => Some(ModelRevocation::InProgress),
            _ => None,
        }
    }

    pub fn refresh(&mut self, value: Option<&str>) -> Vec<ModelQuery> {
        self.confirmation = None;
        let (endpoints, rejected) = candidates(value);
        self.rejected = rejected;
        self.duplicates = 0;
        let mut previous = std::mem::take(&mut self.entries);
        for item in &mut previous {
            let cancelled = item.pending.as_ref().map(|permit| ModelQuery { endpoint: item.endpoint.clone(), permit: permit.clone() });
            let await_completion = item.invalidate().is_some_and(|(_, wait)| wait);
            if let Some(cancelled) = cancelled.filter(|_| await_completion) {
                self.cancelled.push(cancelled);
            }
        }
        let mut entries: Vec<ModelEntry> = Vec::new();
        let mut requests = Vec::new();
        for endpoint in endpoints {
            if entries.iter().any(|item| item.endpoint.address == endpoint.address && item.endpoint.protocol == endpoint.protocol) {
                self.duplicates += 1;
                continue;
            }
            let prior = previous.iter().position(|item| item.endpoint == endpoint).map(|index| previous.remove(index));
            let grant = prior.as_ref().and_then(|item| item.grant.clone());
            let authorized = endpoint.address.ip().is_loopback() || grant.is_some();
            let observation = if authorized { prior.map(|item| item.observation).unwrap_or_default() } else { ModelObservation::default() };
            let mut item = ModelEntry { endpoint, authorized, observation, grant, pending: None };
            if authorized {
                requests.push(item.request());
            }
            entries.push(item);
        }
        self.entries = entries;
        self.confirmations_suspended = false;
        requests
    }

    pub fn suspend_confirmations(&mut self) {
        self.confirmations_suspended = true;
        self.confirmation = None;
    }

    pub fn prepare_authorization(&mut self, endpoint: &Endpoint) -> Option<ModelConfirmation> {
        if self.confirmations_suspended {
            return None;
        }
        let item = self.entries.iter().find(|item| &item.endpoint == endpoint && !item.authorized && !item.endpoint.address.ip().is_loopback())?;
        let confirmation = ModelConfirmation { endpoint: item.endpoint.clone(), nonce: Arc::new(()) };
        self.confirmation = Some(confirmation.clone());
        Some(confirmation)
    }

    /// Mark a request for execution; every dispatched request must later be completed.
    pub fn dispatch(&mut self, query: &ModelQuery) -> bool {
        let valid = self
            .entries
            .iter()
            .any(|item| item.authorized && item.endpoint == query.endpoint && item.pending.as_ref().is_some_and(|permit| Arc::ptr_eq(permit, &query.permit)));
        valid && query.permit.compare_exchange(PREPARED, DISPATCHED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    pub fn cancel_confirmation(&mut self, confirmation: &ModelConfirmation) -> bool {
        if self.confirmation.as_ref() != Some(confirmation) {
            return false;
        }
        self.confirmation = None;
        true
    }

    pub fn confirm(&mut self, confirmation: &ModelConfirmation) -> Option<ModelQuery> {
        if self.confirmations_suspended || self.confirmation.as_ref() != Some(confirmation) {
            return None;
        }
        let item = self.entries.iter_mut().find(|item| item.endpoint == confirmation.endpoint)?;
        if item.authorized || item.endpoint.address.ip().is_loopback() {
            return None;
        }
        item.authorized = true;
        item.grant = Some(ModelGrant { endpoint: item.endpoint.clone(), nonce: confirmation.nonce.clone() });
        let request = item.request();
        self.confirmation = None;
        Some(request)
    }

    pub fn revoke(&mut self, grant: &ModelGrant) -> Option<ModelRevocation> {
        let item = self.entries.iter_mut().find(|item| item.endpoint == grant.endpoint && item.grant.as_ref() == Some(grant))?;
        if !item.authorized || item.endpoint.address.ip().is_loopback() {
            return None;
        }
        item.authorized = false;
        item.grant = None;
        let cancelled = item.pending.as_ref().map(|permit| ModelQuery { endpoint: item.endpoint.clone(), permit: permit.clone() });
        let (outcome, await_completion) = item.invalidate().unwrap_or((ModelRevocation::Idle, false));
        item.observation = ModelObservation::default();
        if let Some(cancelled) = cancelled.filter(|_| await_completion) {
            self.cancelled.push(cancelled);
        }
        self.confirmation = None;
        Some(outcome)
    }

    pub fn finish(&mut self, query: &ModelQuery, result: Result<Vec<String>, ModelError>, at: i64) -> bool {
        self.complete(query, Some(result), at)
    }

    pub fn complete(&mut self, query: &ModelQuery, result: Option<Result<Vec<String>, ModelError>>, at: i64) -> bool {
        if let Some(index) = self.cancelled.iter().position(|cancelled| Arc::ptr_eq(&cancelled.permit, &query.permit)) {
            self.cancelled.remove(index);
            return false;
        }
        let Some(result) = result else { return false };
        let Some(item) = self.entries.iter_mut().find(|item| item.endpoint == query.endpoint) else { return false };
        if !item.authorized || !item.pending.as_ref().is_some_and(|permit| Arc::ptr_eq(permit, &query.permit)) {
            return false;
        }
        if query.permit.compare_exchange(RUNNING, FINISHED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        item.pending = None;
        item.observation.record(result, at);
        true
    }
}

impl Drop for ModelScope {
    fn drop(&mut self) {
        for item in &mut self.entries {
            let _ = item.invalidate();
        }
    }
}

/// Parse an explicitly configured local or private-network endpoint.
pub fn endpoint(value: &str) -> Option<Endpoint> {
    let mut parts = value.split('|');
    let label = parts.next()?.trim();
    let address: SocketAddr = parts.next()?.parse().ok()?;
    let protocol = match parts.next()? {
        "openai" => Protocol::OpenAi,
        "ollama" => Protocol::Ollama,
        _ => return None,
    };
    if parts.next().is_some() || label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) || address.port() == 0 {
        return None;
    }
    let permitted = match address.ip() {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_loopback() || ip.is_private() || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    };
    permitted.then(|| Endpoint { label: label.to_owned(), address, protocol })
}

/// Query model metadata only, with a deadline and response-size limit.
pub fn models(address: SocketAddr, protocol: Protocol, timeout: Duration) -> Result<Vec<String>, ModelError> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ModelError::Unavailable)?;
    runtime.block_on(async {
        match tokio::time::timeout(timeout, query(address, protocol)).await {
            Ok(result) => result,
            Err(_) => Err(ModelError::TimedOut),
        }
    })
}

async fn query(address: SocketAddr, protocol: Protocol) -> Result<Vec<String>, ModelError> {
    let stream = tokio::net::TcpStream::connect(address).await.map_err(|_| ModelError::Unavailable)?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|_| ModelError::Unavailable)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let path = match protocol {
        Protocol::OpenAi => "/v1/models",
        Protocol::Ollama => "/api/tags",
    };
    let request = hyper::Request::get(path)
        .header(hyper::header::HOST, address.to_string())
        .header(hyper::header::CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|_| ModelError::InvalidResponse)?;
    let response = sender.send_request(request).await.map_err(|_| ModelError::Unavailable)?;
    if !response.status().is_success() {
        return Err(ModelError::Http(response.status().as_u16()));
    }
    let body = Limited::new(response.into_body(), 256 * 1024).collect().await.map_err(|_| ModelError::InvalidResponse)?.to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|_| ModelError::InvalidResponse)?;
    let (key, name) = match protocol {
        Protocol::OpenAi => ("data", "id"),
        Protocol::Ollama => ("models", "name"),
    };
    let entries = json.get(key).and_then(|v| v.as_array()).filter(|v| v.len() <= 512).ok_or(ModelError::InvalidResponse)?;
    entries
        .iter()
        .map(|entry| {
            entry.get(name).and_then(|v| v.as_str()).filter(|name| !name.is_empty() && name.len() <= 256).map(str::to_owned).ok_or(ModelError::InvalidResponse)
        })
        .collect()
}
