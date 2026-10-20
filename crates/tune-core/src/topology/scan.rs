use super::{Failure, FailureCode};

/// Validate already bounded adapter output. Discovery lives in the transport
/// adapters so there is only one walker with a single set of safety limits.
pub fn bounded_text(text: &[u8], limit: usize) -> Result<&str, Failure> {
    if text.len() > limit {
        return Err(Failure::new(FailureCode::LimitReached, "read budget exceeded"));
    }
    std::str::from_utf8(text).map_err(|_| Failure::new(FailureCode::ParseError, "input is not utf-8"))
}
