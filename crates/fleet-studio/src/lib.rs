//! Read-only presentation of existing Katala Tune observations.

pub mod audit;
pub mod confirmation_keys;
pub mod llm;
pub mod llm_keyboard;
pub mod navigation;
pub mod pointer_id;
pub mod recent;
pub mod refresh;
pub mod scope;
pub mod search_input;
pub mod source;
pub mod startup_selection;
pub mod ui_consent;

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
