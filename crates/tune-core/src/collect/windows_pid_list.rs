//! Bounded PID-list validation. Partial lists never establish member absence.
use std::io;

pub(super) const MAX_ACTIVE: usize = 256;
const PID_LIST_QUERIES: usize = 4;

#[repr(C)]
pub(super) struct PidList {
    pub assigned: u32,
    pub count: u32,
    pub pids: [usize; MAX_ACTIVE],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Reason {
    AssignedCountMismatch,
    CountOutOfBounds,
    IdentifierOutOfRange,
    ZeroIdentifier,
    DuplicateIdentifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ValidationError {
    pub reason: Reason,
    pub assigned: u32,
    pub count: u32,
}

#[derive(Debug)]
pub(super) enum ReadError {
    Query(io::Error),
    Validation(ValidationError),
}

pub(super) fn complete_pid_list(assigned: u32, count: u32, pids: &[usize]) -> Result<Vec<u32>, ValidationError> {
    let error = |reason| ValidationError { reason, assigned, count };
    if assigned as usize > MAX_ACTIVE || count as usize > MAX_ACTIVE || count as usize > pids.len() {
        return Err(error(Reason::CountOutOfBounds));
    }
    if count > assigned {
        return Err(error(Reason::AssignedCountMismatch));
    }
    let mut result = Vec::with_capacity(count as usize);
    for value in &pids[..count as usize] {
        let pid = u32::try_from(*value).map_err(|_| error(Reason::IdentifierOutOfRange))?;
        if pid == 0 || result.contains(&pid) {
            return Err(error(if pid == 0 { Reason::ZeroIdentifier } else { Reason::DuplicateIdentifier }));
        }
        result.push(pid);
    }
    if assigned != count {
        return Err(error(Reason::AssignedCountMismatch));
    }
    Ok(result)
}

pub(super) fn read_complete_pid_list(mut query: impl FnMut(&mut PidList) -> io::Result<()>) -> Result<Vec<u32>, ReadError> {
    // A few immediate observations tolerate transient incompleteness without
    // sleeping, growing the resource cap, or adding/extending any deadline.
    let mut remaining = PID_LIST_QUERIES;
    loop {
        let mut pids = PidList { assigned: 0, count: 0, pids: [0; MAX_ACTIVE] };
        query(&mut pids).map_err(ReadError::Query)?;
        match complete_pid_list(pids.assigned, pids.count, &pids.pids) {
            Ok(members) => return Ok(members),
            Err(error) if error.reason == Reason::AssignedCountMismatch && pids.count < pids.assigned && remaining > 1 => remaining -= 1,
            Err(error) => return Err(ReadError::Validation(error)),
        }
        // A valid partial list is still unknown membership. Discard it entirely;
        // only a later complete query may establish that a candidate is absent.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_then_complete_query_returns_only_the_complete_membership() {
        // Returning [17] here would incorrectly prove that candidate 23 vanished.
        for complete_on in [2, 4] {
            let mut calls = 0;
            let members = read_complete_pid_list(|snapshot| {
                calls += 1;
                snapshot.assigned = 2;
                snapshot.count = if calls < complete_on { 1 } else { 2 };
                snapshot.pids[..2].copy_from_slice(&[17, 23]);
                Ok(())
            })
            .unwrap();
            assert_eq!(members, vec![17, 23]);
            assert_eq!(calls, complete_on);
        }
    }

    #[test]
    fn incomplete_empty_list_cannot_establish_no_membership() {
        let mut calls = 0;
        let members = read_complete_pid_list(|snapshot| {
            calls += 1;
            snapshot.assigned = 1;
            if calls > 1 {
                snapshot.count = 1;
                snapshot.pids[0] = 23;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(members, vec![23]);
        assert_eq!(calls, 2);
    }

    #[test]
    fn five_assigned_four_returned_can_be_followed_by_complete_four() {
        // The CI failure had sufficient buffer capacity and a successful query
        // with assigned=5, returned=4. The next complete list can be smaller.
        let mut calls = 0;
        let members = read_complete_pid_list(|snapshot| {
            calls += 1;
            snapshot.assigned = if calls == 1 { 5 } else { 4 };
            snapshot.count = 4;
            snapshot.pids[..4].copy_from_slice(&[17, 23, 31, 41]);
            Ok(())
        })
        .unwrap();
        assert_eq!(members, vec![17, 23, 31, 41]);
        assert_eq!(calls, 2);
    }

    #[test]
    fn persistent_partial_membership_fails_after_four_queries() {
        let mut calls = 0;
        let error = read_complete_pid_list(|snapshot| {
            calls += 1;
            snapshot.assigned = calls + 1;
            snapshot.count = calls;
            snapshot.pids[..calls as usize].copy_from_slice(&[17, 23, 31, 41][..calls as usize]);
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(error, ReadError::Validation(ValidationError { reason: Reason::AssignedCountMismatch, assigned: 5, count: 4 })));
        assert_eq!(calls, 4, "persistent incompleteness must neither escape nor query indefinitely");
    }

    #[test]
    fn malformed_partial_membership_is_rejected_before_another_query() {
        for (assigned, count, values, reason) in [
            (2, 1, vec![0], Reason::ZeroIdentifier),
            (3, 2, vec![17, 17], Reason::DuplicateIdentifier),
            (2, 1, vec![usize::MAX], Reason::IdentifierOutOfRange),
            (257, 1, vec![17], Reason::CountOutOfBounds),
            (1, 2, vec![17, 23], Reason::AssignedCountMismatch),
            (256, 257, vec![], Reason::CountOutOfBounds),
        ] {
            let mut calls = 0;
            let error = read_complete_pid_list(|snapshot| {
                calls += 1;
                snapshot.assigned = assigned;
                snapshot.count = count;
                snapshot.pids[..values.len()].copy_from_slice(&values);
                Ok(())
            })
            .unwrap_err();
            assert!(
                matches!(error, ReadError::Validation(ValidationError { reason: actual, assigned: a, count: c }) if actual == reason && a == assigned && c == count)
            );
            assert_eq!(calls, 1, "malformed data is not retryable incompleteness");
        }
    }

    #[test]
    fn query_failure_is_returned_immediately_including_after_a_partial_list() {
        for partial_first in [false, true] {
            let mut calls = 0;
            let error = read_complete_pid_list(|snapshot| {
                calls += 1;
                if partial_first && calls == 1 {
                    snapshot.assigned = 2;
                    snapshot.count = 1;
                    snapshot.pids[0] = 17;
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(5))
                }
            })
            .unwrap_err();
            assert!(matches!(error, ReadError::Query(error) if error.raw_os_error() == Some(5)));
            assert_eq!(calls, if partial_first { 2 } else { 1 });
        }
    }

    #[test]
    fn complete_or_empty_membership_needs_no_additional_query() {
        for (values, expected) in [(vec![], vec![]), (vec![17, 23], vec![17, 23])] {
            let mut calls = 0;
            let members = read_complete_pid_list(|snapshot| {
                calls += 1;
                assert_eq!(calls, 1, "a complete list must not be queried again");
                snapshot.assigned = values.len() as u32;
                snapshot.count = values.len() as u32;
                snapshot.pids[..values.len()].copy_from_slice(&values);
                Ok(())
            })
            .unwrap();
            assert_eq!(members, expected);
        }
    }

    #[test]
    fn incomplete_or_malformed_pid_lists_cannot_prove_absence() {
        assert_eq!(complete_pid_list(2, 2, &[17, 23]).unwrap(), vec![17, 23]);
        assert!(complete_pid_list(2, 1, &[17]).is_err(), "missing candidate in a partial list is not absence");
        assert!(complete_pid_list(1, 2, &[17, 23]).is_err());
        assert!(complete_pid_list(2, 2, &[17]).is_err());
        assert!(complete_pid_list(0, 0, &[]).unwrap().is_empty());
        assert!(complete_pid_list(1, 1, &[0]).is_err());
        assert!(complete_pid_list(2, 2, &[17, 17]).is_err());
        assert!(complete_pid_list(1, 1, &[usize::MAX]).is_err());
        assert!(complete_pid_list(257, 257, &[17; 257]).is_err());
    }
}
