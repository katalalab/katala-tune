use std::cell::RefCell;
use std::rc::Rc;

use fleet_studio::scope::Grants;
use fleet_studio::source::{self, MachineReading, SavedReadError, SavedReader, SavedState};
use fleet_studio::startup_selection::{Candidate, PendingSelection, parse_args};
use serde_json::json;
use tune_core::nodes::Node;

#[derive(Clone)]
struct FakeReader(Rc<RefCell<Vec<String>>>);

impl SavedReader for FakeReader {
    fn read_machine(&self, node_id: &str) -> Result<MachineReading, SavedReadError> {
        self.0.borrow_mut().push(node_id.into());
        Ok(MachineReading { last_at: Some(9_000), ..Default::default() })
    }
}

#[test]
fn changed_requested_row_is_not_read_and_other_selected_row_remains_available() {
    let host = "fixture-host";
    let original: Vec<Node> =
        [json!({"id":"one","role":"first"}), json!({"id":"two","role":"second"})].iter().map(|value| Node::from_value(value, host)).collect();
    let mut grants = Grants::default();
    let first = source::project_nodes(&original, None, host, 10_000, &grants, || -> Result<FakeReader, SavedReadError> {
        panic!("ungranted data source must not open")
    });
    let candidates: Vec<_> = first.nodes.iter().map(|node| Candidate { id: &node.id, fingerprint: node.fingerprint.as_deref() }).collect();
    let request = parse_args(["--saved-summary=one".into(), "--saved-summary=two".into()]);
    let mut pending = PendingSelection::new(request);
    assert_eq!(pending.apply(&candidates, &mut grants), Ok(2));
    let old_two = grants.matching("two", candidates[1].fingerprint.unwrap()).unwrap();

    let mut changed = original.clone();
    changed[1] = Node::from_value(&json!({"id":"two","role":"changed"}), host);
    let reads = Rc::new(RefCell::new(Vec::new()));
    let reader = FakeReader(reads.clone());
    let mut result = source::project_nodes(&changed, None, host, 10_000, &grants, || Ok(reader));
    source::revalidate_dashboard(&mut result, Some(&changed), None, None, host);
    let fingerprints = result.nodes.iter().filter_map(|node| node.fingerprint.as_ref().map(|fp| (node.id.clone(), fp.clone()))).collect();
    grants.reconcile(&fingerprints);
    source::discard_revoked(&mut result, &grants);
    assert_eq!(&*reads.borrow(), &["one"]);
    assert_eq!(result.nodes[0].last_at, Some(9_000));
    assert_eq!(result.nodes[1].saved_state, SavedState::Unapproved);
    assert!(!old_two.is_active());
    let changed_cards: Vec<_> = result.nodes.iter().map(|node| Candidate { id: &node.id, fingerprint: node.fingerprint.as_deref() }).collect();
    assert_eq!(pending.apply(&changed_cards, &mut grants), Ok(0));
}

#[test]
fn selected_row_changed_during_read_hides_all_returned_values() {
    let host = "fixture-host";
    let original: Vec<Node> =
        [json!({"id":"one","role":"first"}), json!({"id":"two","role":"second"})].iter().map(|value| Node::from_value(value, host)).collect();
    let mut grants = Grants::default();
    let first = source::project_nodes(&original, None, host, 10_000, &grants, || -> Result<FakeReader, SavedReadError> {
        panic!("ungranted data source must not open")
    });
    let candidates: Vec<_> = first.nodes.iter().map(|node| Candidate { id: &node.id, fingerprint: node.fingerprint.as_deref() }).collect();
    let mut pending = PendingSelection::new(parse_args(["--saved-summary=one".into(), "--saved-summary=two".into()]));
    assert_eq!(pending.apply(&candidates, &mut grants), Ok(2));
    let one = grants.matching("one", candidates[0].fingerprint.unwrap()).unwrap();
    let two = grants.matching("two", candidates[1].fingerprint.unwrap()).unwrap();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let mut result = source::project_nodes(&original, None, host, 10_000, &grants, || Ok(FakeReader(reads.clone())));
    let mut changed = original.clone();
    changed[1] = Node::from_value(&json!({"id":"two","role":"changed"}), host);
    source::revalidate_dashboard(&mut result, Some(&changed), None, None, host);
    assert_eq!(&*reads.borrow(), &["one", "two"]);
    assert!(result.nodes.iter().all(|node| node.last_at.is_none()));
    assert!(result.config_error.is_some());
    assert!(!one.is_active());
    assert!(!two.is_active());
}
