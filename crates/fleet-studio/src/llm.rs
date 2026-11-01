use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    OpenAi,
    Ollama,
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

pub fn configured(value: Option<&str>) -> (Vec<Endpoint>, usize) {
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
