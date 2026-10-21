//! Narrow response DTOs ignore unknown data (including Env, Labels and daemon messages).
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
// deserialize_any rejects missing fields; explicit null remains a known empty catalog.
struct RequiredNullable<T>(Option<T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for RequiredNullable<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
            type Value = RequiredNullable<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("explicit catalog or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(RequiredNullable(None))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::SeqAccessDeserializer::new(seq)).map(|v| RequiredNullable(Some(v)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(|v| RequiredNullable(Some(v)))
            }
        }
        deserializer.deserialize_any(Visitor(std::marker::PhantomData))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Container {
    id: String,
    state: State,
    host_config: HostConfig,
    mounts: RequiredNullable<Vec<Mount>>,
    network_settings: NetworkSettings,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct State {
    status: Option<String>,
    health: Option<Health>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Health {
    status: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HostConfig {
    memory: Option<u64>,
    nano_cpus: Option<u64>,
    cpu_period: Option<i64>,
    cpu_quota: Option<i64>,
    cpu_shares: Option<u64>,
    cpuset_cpus: Option<String>,
    restart_policy: Option<Restart>,
    log_config: Option<Log>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Restart {
    name: Option<String>,
    maximum_retry_count: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Log {
    #[serde(rename = "Type")]
    kind: Option<String>,
    config: Option<LogOptions>,
}
#[derive(Deserialize)]
struct LogOptions {
    #[serde(rename = "max-size")]
    size: Option<String>,
    #[serde(rename = "max-file")]
    file: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    source: Option<String>,
    destination: String,
    #[serde(rename = "RW")]
    writable: Option<bool>,
    name: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NetworkSettings {
    networks: RequiredNullable<BTreeMap<String, Membership>>,
    ports: RequiredNullable<BTreeMap<String, Option<Vec<Port>>>>,
}
#[derive(Deserialize)]
struct Membership {
    #[serde(rename = "NetworkID")]
    id: String,
}
#[derive(Deserialize)]
struct Port {
    #[serde(rename = "HostPort")]
    port: String,
}
impl Container {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
    pub(super) fn sanitize(self) -> Result<super::super::docker::Container, &'static str> {
        let host = self.host_config;
        if self.mounts.0.as_ref().map_or(0, Vec::len) > 128 || self.network_settings.networks.0.as_ref().is_some_and(|n| n.len() > 128) {
            return Err("Docker member count exceeded");
        }
        let ports = self
            .network_settings
            .ports
            .0
            .unwrap_or_default()
            .into_iter()
            .flat_map(|(key, ports)| ports.unwrap_or_default().into_iter().map(move |p| json!({"container":key,"host":p.port})))
            .take(129)
            .collect::<Vec<_>>();
        if ports.len() > 128 {
            return Err("Docker port count exceeded");
        }
        let mounts = self
            .mounts
            .0
            .unwrap_or_default()
            .into_iter()
            .map(|m| json!({"kind":m.kind,"source":m.source,"target":m.destination,"rw":m.writable,"name":m.name}))
            .collect::<Vec<_>>();
        super::super::docker::parse_container(
            &json!({"id":self.id,"state":self.state.status,"health":self.state.health.and_then(|h|h.status),"memory":host.memory,"nano_cpus":host.nano_cpus,"cpu_period":host.cpu_period,"cpu_quota":host.cpu_quota,"cpu_shares":host.cpu_shares,"cpuset":host.cpuset_cpus,"restart":host.restart_policy.as_ref().and_then(|r|r.name.as_ref()),"restart_max":host.restart_policy.and_then(|r|r.maximum_retry_count),"log_driver":host.log_config.as_ref().and_then(|l|l.kind.as_ref()),"max_size":host.log_config.as_ref().and_then(|l|l.config.as_ref()).and_then(|o|o.size.as_ref()),"max_file":host.log_config.as_ref().and_then(|l|l.config.as_ref()).and_then(|o|o.file.as_ref()),"mounts":mounts,"networks":self.network_settings.networks.0.unwrap_or_default().into_values().map(|n|n.id).collect::<Vec<_>>(),"ports":ports}),
        )
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Network {
    id: String,
    driver: Option<String>,
    internal: Option<bool>,
}
impl Network {
    pub(super) fn sanitize(self) -> Result<super::super::docker::Network, &'static str> {
        if self.id.len() != 64 || !self.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid Docker network identity");
        }
        Ok(super::super::docker::Network {
            id: crate::db::sha256_hex(self.id.as_bytes()),
            driver: self.driver.filter(|s| ["bridge", "host", "null", "overlay", "macvlan", "ipvlan"].contains(&s.as_str())),
            internal: self.internal,
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Volume {
    name: String,
    driver: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Volumes {
    volumes: Catalog,
    warnings: Option<Vec<serde::de::IgnoredAny>>,
}
// Required wrapper distinguishes a missing catalog from Docker's explicit null empty list.
struct Catalog(Option<Vec<Volume>>);
impl<'de> Deserialize<'de> for Catalog {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Catalog;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("explicit volume array or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Catalog, E> {
                Ok(Catalog(None))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Catalog, A::Error> {
                let mut volumes = vec![];
                while let Some(volume) = seq.next_element()? {
                    if volumes.len() >= 512 {
                        return Err(serde::de::Error::custom("volume count exceeded"));
                    }
                    volumes.push(volume);
                }
                Ok(Catalog(Some(volumes)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
impl Volumes {
    pub(super) fn sanitize(self, max: usize) -> Result<Vec<super::super::docker::Volume>, &'static str> {
        if self.warnings.is_some_and(|warnings| !warnings.is_empty()) {
            return Err("Docker volume catalog warnings");
        }
        let volumes = self.volumes.0.unwrap_or_default();
        if volumes.len() > max {
            return Err("Docker volume count exceeded");
        }
        volumes
            .into_iter()
            .map(|v| {
                if v.name.is_empty() || v.name.len() > 4096 || v.name.chars().any(char::is_control) {
                    return Err("invalid Docker volume identity");
                }
                Ok(super::super::docker::Volume { id: crate::db::sha256_hex(v.name.as_bytes()), driver: v.driver.filter(|s| s == "local") })
            })
            .collect()
    }
}
