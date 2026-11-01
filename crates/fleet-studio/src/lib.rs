//! Read-only presentation of existing Katala Tune observations.

pub mod audit;
pub mod llm;
pub mod navigation;
pub mod source;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observation {
    Current,
    Stale,
    Failed,
    Unknown,
    Excluded,
}

/// Classify a stored observation without treating missing or future data as healthy.
pub fn observation(at: Option<i64>, failed: bool, active: bool, now: i64, max_age_ms: i64) -> Observation {
    if !active {
        return Observation::Excluded;
    }
    if failed {
        return Observation::Failed;
    }
    match at {
        Some(at) if max_age_ms >= 0 && at >= 0 && at <= now => {
            if now.saturating_sub(at) > max_age_ms {
                Observation::Stale
            } else {
                Observation::Current
            }
        }
        _ => Observation::Unknown,
    }
}
