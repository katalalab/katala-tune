use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use fleet_studio::llm::{ModelError, Protocol, endpoint, models};

#[test]
fn configured_endpoints_are_explicit_private_addresses() {
    let endpoint = endpoint("fixture|127.0.0.1:18080|openai").unwrap();
    assert_eq!(endpoint.protocol, Protocol::OpenAi);
    assert!(fleet_studio::llm::endpoint("fixture|8.8.8.8:80|openai").is_none());
    assert!(fleet_studio::llm::endpoint("fixture|127.0.0.1:18080|unknown").is_none());
    assert!(fleet_studio::llm::endpoint("fixture|localhost:18080|openai").is_none());
}

#[test]
fn invalid_configured_endpoint_is_reported_instead_of_disappearing() {
    let (endpoints, invalid) = fleet_studio::llm::configured(Some("fixture|127.0.0.1:18080|openai;broken"));
    assert_eq!(endpoints.len(), 4);
    assert_eq!(invalid, 1);
}

#[test]
fn failed_refresh_keeps_previous_models_with_their_own_timestamp() {
    let mut observation = fleet_studio::llm::ModelObservation::default();
    observation.record(Ok(vec!["fixture-model".into()]), 100);
    observation.record(Err(ModelError::TimedOut), 200);
    assert_eq!(observation.latest, Some(Err(ModelError::TimedOut)));
    assert_eq!(observation.checked_at, Some(200));
    assert_eq!(observation.last_good, Some((vec!["fixture-model".into()], 100)));
}

fn server(response: &'static str, expected_path: &'static str) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 2048];
        let size = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..size]).starts_with(&format!("GET {expected_path} ")));
        stream.write_all(response.as_bytes()).unwrap();
    });
    (address, thread)
}

#[test]
fn openai_model_listing_uses_get_without_running_a_model() {
    let body = r#"{"data":[{"id":"fixture-model"}]}"#;
    let response = Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_boxed_str());
    let (address, server) = server(response, "/v1/models");
    assert_eq!(models(address, Protocol::OpenAi, Duration::from_secs(2)), Ok(vec!["fixture-model".to_owned()]));
    server.join().unwrap();
}

#[test]
fn invalid_json_is_not_treated_as_an_empty_model_list() {
    let (address, server) = server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}", "/api/tags");
    assert_eq!(models(address, Protocol::Ollama, Duration::from_secs(2)), Err(ModelError::InvalidResponse));
    server.join().unwrap();
}

#[test]
fn slow_server_is_reported_as_a_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(250));
    });
    let started = std::time::Instant::now();
    assert_eq!(models(address, Protocol::OpenAi, Duration::from_millis(75)), Err(ModelError::TimedOut));
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}
