use fleet_studio::{Observation, observation};

#[test]
fn missing_or_future_observations_are_unknown() {
    assert_eq!(observation(None, false, true, 10_000, 1_000), Observation::Unknown);
    assert_eq!(observation(Some(-1), false, true, 10_000, 1_000), Observation::Unknown);
    assert_eq!(observation(Some(10_001), false, true, 10_000, 1_000), Observation::Unknown);
}

#[test]
fn newer_failure_remains_visible_even_with_an_earlier_success() {
    assert_eq!(observation(Some(9_999), true, true, 10_000, 1_000), Observation::Failed);
}

#[test]
fn older_success_is_stale_and_recent_success_is_current() {
    assert_eq!(observation(Some(8_999), false, true, 10_000, 1_000), Observation::Stale);
    assert_eq!(observation(Some(9_000), false, true, 10_000, 1_000), Observation::Current);
}

#[test]
fn excluded_machine_is_not_treated_as_a_failed_online_machine() {
    assert_eq!(observation(None, true, false, 10_000, 1_000), Observation::Excluded);
}
