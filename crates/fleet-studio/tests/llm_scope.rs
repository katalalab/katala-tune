use fleet_studio::llm;

#[test]
fn configured_defaults_and_loopback_are_automatic_but_remote_is_not() {
    let (endpoints, rejected) = llm::configured(Some("local sample|127.0.0.1:12345|openai;remote sample|10.0.0.1:12345|ollama"));

    assert_eq!(rejected, 0);
    assert_eq!(endpoints.len(), 4);
    assert!(endpoints.iter().all(|endpoint| endpoint.address.ip().is_loopback()));
    assert!(endpoints.iter().any(|endpoint| endpoint.label == "local sample"));
}
