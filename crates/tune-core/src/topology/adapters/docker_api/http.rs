//! One HTTP/1 driver per verified local stream. Dropping the owner drops all I/O.
use super::{DirectConfig, peer};
use crate::topology::{
    TopologyLimits,
    adapters::docker::{DockerEngine, DockerPacket},
};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::{Request, body::Bytes};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Instant,
};

struct Meter {
    bytes: usize,
    cap: usize,
    digest: Sha256,
}
struct Wire<T> {
    io: T,
    meter: Arc<Mutex<Meter>>,
}
impl<T: AsyncRead + Unpin> AsyncRead for Wire<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, out: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let remaining = {
            let m = self.meter.lock().unwrap();
            m.cap.saturating_sub(m.bytes)
        };
        let mut bytes = [0u8; 8192];
        let take = out.remaining().min(bytes.len()).min(remaining.saturating_add(1));
        let mut read = ReadBuf::new(&mut bytes[..take]);
        match Pin::new(&mut self.io).poll_read(cx, &mut read) {
            Poll::Ready(Ok(())) => {
                let data = read.filled();
                let mut meter = self.meter.lock().unwrap();
                if data.len() > remaining {
                    return Poll::Ready(Err(std::io::Error::other("Docker wire budget exceeded")));
                }
                meter.bytes += data.len();
                meter.digest.update(data);
                out.put_slice(data);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Wire<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}
/// Only this module constructs an observation; it retains same-connection peer handles.
pub(super) struct Observation {
    pub(super) packet: DockerPacket,
    guards: Vec<peer::Guard>,
    pub(super) digest: String,
    pub(super) bytes: usize,
}
impl Observation {
    pub(super) fn check(&self) -> Result<(), &'static str> {
        for guard in &self.guards {
            guard.check()?;
        }
        Ok(())
    }
}
enum Get<'a> {
    Version,
    Info(&'a str),
    Containers(&'a str),
    Inspect(&'a str, &'a str),
    Networks(&'a str),
    Volumes(&'a str),
}
impl Get<'_> {
    fn path(&self) -> String {
        match self {
            Self::Version => "/version".into(),
            Self::Info(v) => format!("/v{v}/info"),
            Self::Containers(v) => format!("/v{v}/containers/json?all=1"),
            Self::Inspect(v, id) => format!("/v{v}/containers/{id}/json"),
            Self::Networks(v) => format!("/v{v}/networks"),
            Self::Volumes(v) => format!("/v{v}/volumes"),
        }
    }
}
struct Session {
    sender: hyper::client::conn::http1::SendRequest<Empty<Bytes>>,
    body_cap: usize,
    requests: usize,
    deadline: Instant,
    transcript: Sha256,
}
impl Session {
    async fn get<T: serde::de::DeserializeOwned>(&mut self, get: Get<'_>) -> Result<T, &'static str> {
        if Instant::now() >= self.deadline || self.requests >= 520 {
            return Err("Docker request deadline or count exceeded");
        }
        self.requests += 1;
        let path = get.path();
        self.transcript.update(path.as_bytes());
        let request =
            Request::builder().method("GET").uri(path).header("host", "localhost").body(Empty::<Bytes>::new()).map_err(|_| "invalid fixed request")?;
        let response = self.sender.send_request(request).await.map_err(|_| "Docker HTTP response unavailable")?;
        let valid = response.status() == hyper::StatusCode::OK
            && !response.headers().contains_key("content-encoding")
            && !response.headers().contains_key("upgrade")
            && !response.headers().contains_key("location");
        if response
            .headers()
            .get(hyper::header::CONTENT_LENGTH)
            .is_some_and(|v| v.to_str().ok().and_then(|s| s.parse::<u64>().ok()).is_none_or(|n| n > self.body_cap as u64))
        {
            return Err("Docker body budget exceeded");
        }
        let body = Limited::new(response.into_body(), self.body_cap).collect().await.map_err(|_| "Docker body incomplete or exceeded budget")?.to_bytes();
        self.transcript.update(&body);
        if !valid {
            return Err("unsupported Docker HTTP response");
        }
        if Instant::now() >= self.deadline {
            return Err("Docker request deadline exceeded");
        }
        serde_json::from_slice(&body).map_err(|_| "invalid Docker API projection")
    }
}
#[derive(serde::Deserialize)]
struct Version {
    #[serde(rename = "ApiVersion")]
    api: String,
    #[serde(rename = "MinAPIVersion")]
    minimum: Option<String>,
}
#[derive(serde::Deserialize)]
struct Info {
    #[serde(rename = "ID")]
    id: String,
}
#[derive(serde::Deserialize)]
struct Id {
    #[serde(rename = "Id")]
    id: String,
}
fn version_pair(text: &str) -> Option<(u16, u16)> {
    let (a, b) = text.split_once('.')?;
    if a.len() > 2 || b.len() > 3 || !a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((a.parse().ok()?, b.parse().ok()?))
}
pub(super) async fn observe(config: &DirectConfig, limits: &TopologyLimits, deadline: Instant) -> Result<Observation, &'static str> {
    let operation = async {
        if config.endpoints.is_empty() || config.endpoints.len() > 8 {
            return Err("invalid explicit Docker endpoint selection");
        }
        let cap = limits.total_bytes_per_environment.min(8 * 1024 * 1024);
        let meter = Arc::new(Mutex::new(Meter { bytes: 0, cap, digest: Sha256::new() }));
        let mut packet = DockerPacket { engines: vec![] };
        let mut guards = vec![];
        let mut transcript = Sha256::new();
        for policy in &config.endpoints {
            let (io, guard) = peer::connect(policy).await?;
            transcript.update(guard.epoch_digest().as_bytes());
            let mut builder = hyper::client::conn::http1::Builder::new();
            builder.max_headers(32).max_header_size(16384).max_buf_size(32768);
            let (sender, mut driver) =
                builder.handshake(TokioIo::new(Wire { io, meter: meter.clone() })).await.map_err(|_| "Docker HTTP connection unavailable")?;
            let known = packet.engines.iter().map(|e| e.id.clone()).collect::<std::collections::HashSet<_>>();
            let read = async {
                let mut session =
                    Session { sender, body_cap: limits.file_bytes.min(limits.stdout_bytes).min(1024 * 1024), requests: 0, deadline, transcript: Sha256::new() };
                let version: Version = session.get(Get::Version).await?;
                let supported = (1, 45);
                let maximum = version_pair(&version.api).ok_or("invalid Docker API version")?;
                let minimum = version.minimum.as_deref().map(version_pair).unwrap_or(Some((1, 24))).ok_or("invalid Docker API version")?;
                if minimum > supported || maximum < (1, 24) || minimum > maximum {
                    return Err("unsupported Docker API version");
                }
                let selected = maximum.min(supported);
                let version = format!("{}.{}", selected.0, selected.1);
                let info: Info = session.get(Get::Info(&version)).await?;
                if info.id.is_empty() || info.id.len() > 4096 || info.id.chars().any(char::is_control) {
                    return Err("invalid Docker engine identity");
                }
                let id = crate::db::sha256_hex(info.id.as_bytes());
                if known.contains(&id) {
                    return Err("duplicate Docker engine identity");
                }
                let containers: Vec<Id> = session.get(Get::Containers(&version)).await?;
                if containers.len() > 512.min(limits.entries_per_environment) {
                    return Err("Docker container count exceeded");
                }
                let mut ids = std::collections::HashSet::new();
                let mut engine = DockerEngine { id, containers: vec![], networks: vec![], volumes: vec![] };
                for row in containers {
                    if row.id.len() != 64 || !row.id.bytes().all(|b| b.is_ascii_hexdigit()) || !ids.insert(row.id.clone()) {
                        return Err("invalid or duplicate Docker container identity");
                    }
                    let raw: super::projection::Container = session.get(Get::Inspect(&version, &row.id)).await?;
                    if raw.id() != row.id {
                        return Err("Docker inspect identity mismatch");
                    }
                    engine.containers.push(raw.sanitize()?);
                }
                let networks: Vec<super::projection::Network> = session.get(Get::Networks(&version)).await?;
                if networks.len() > 512.min(limits.entries_per_environment) {
                    return Err("Docker network count exceeded");
                }
                engine.networks = networks.into_iter().map(|n| n.sanitize()).collect::<Result<_, _>>()?;
                let volumes: super::projection::Volumes = session.get(Get::Volumes(&version)).await?;
                engine.volumes = volumes.sanitize(512.min(limits.entries_per_environment))?;
                Ok((Some(engine), session.transcript))
            };
            // The driver is polled in this owner future, never spawned or detached.
            tokio::pin!(read);
            let (engine, request_digest) = tokio::select! { biased; result=&mut read=>result, _=&mut driver=>read.await }?;
            guard.check()?;
            if let Some(engine) = engine {
                packet.engines.push(engine);
            }
            transcript.update(request_digest.finalize());
            guards.push(guard);
        }
        if Instant::now() >= deadline {
            return Err("Docker batch deadline exceeded");
        }
        let meter = meter.lock().unwrap();
        transcript.update(meter.digest.clone().finalize());
        let digest = crate::db::sha256_hex(&transcript.finalize());
        let observation = Observation { packet, guards, digest, bytes: meter.bytes };
        observation.check()?;
        Ok(observation)
    };
    tokio::time::timeout_at(deadline, operation).await.map_err(|_| "Docker batch deadline exceeded")?
}
