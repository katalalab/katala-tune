//! One background reader per window. The watch mailbox coalesces navigation
//! requests; a slow query cannot create an unbounded queue of database readers.
use crate::{
    dashboard::DashboardView,
    model::{self, NodeView},
    panels::{self, LogLevel, Page, Panel},
};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tune_core::{engine::Engine, nodes};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Page(Page),
    Node(String),
}
#[derive(Clone)]
pub struct Request {
    pub generation: u64,
    pub route: Route,
    pub query: String,
    pub log_level: LogLevel,
    pub minutes: i64,
}
pub struct Response {
    pub generation: u64,
    pub nodes: Result<Vec<NodeView>, String>,
    pub panel: Option<Result<Panel, String>>,
    pub dashboard: Option<Result<DashboardView, String>>,
    pub config_error: Option<String>,
}
pub struct Reader {
    sender: watch::Sender<Option<Request>>,
    result: Arc<Mutex<Option<Response>>>,
}
impl Reader {
    pub fn new(engine: Arc<Engine>, rt: &tokio::runtime::Runtime) -> Self {
        let (sender, mut incoming) = watch::channel::<Option<Request>>(None);
        let result = Arc::new(Mutex::new(None));
        let output = result.clone();
        rt.spawn(async move {
            while incoming.changed().await.is_ok() {
                let Some(request) = incoming.borrow_and_update().clone() else {
                    continue;
                };
                let engine = engine.clone();
                let generation = request.generation;
                let dashboard_requested = request.route == Route::Page(Page::Dashboard);
                let response = tokio::task::spawn_blocking(move || {
                    let cfg = engine.reload_config();
                    if let Some(error) = engine.config_error() {
                        return Response {
                            generation,
                            nodes: Err(error.clone()),
                            panel: (!dashboard_requested).then(|| Err(error.clone())),
                            dashboard: dashboard_requested.then(|| Err(error.clone())),
                            config_error: Some(error),
                        };
                    }
                    let ledger: Vec<_> = cfg.nodes.iter().map(nodes::Node::public).collect();
                    let nodes = engine.last().and_then(|last| {
                        let mut nodes = model::build(&ledger, &last);
                        for n in &mut nodes {
                            let rows = engine.with_db(|d| d.history(&n.id, 60))?;
                            n.score_trend = model::series(&rows, "score");
                            n.cpu_trend = model::series(&rows, "cpu");
                            n.mem_trend = model::series(&rows, "mem_avail");
                        }
                        Ok(nodes)
                    });
                    let dashboard = dashboard_requested.then(|| crate::dashboard::load_saved(&engine, request.minutes));
                    let panel = match request.route {
                        Route::Page(page) if !matches!(page, Page::Overview | Page::Dashboard) => {
                            Some(panels::load(&engine, page, &request.query, request.log_level))
                        }
                        _ => None,
                    };
                    Response { generation, nodes, panel, dashboard, config_error: engine.config_error() }
                })
                .await
                .unwrap_or_else(|_| Response {
                    generation,
                    nodes: Err("保存データの読み込みタスクが停止しました".into()),
                    panel: None,
                    dashboard: dashboard_requested.then(|| Err("保存データの読み込みタスクが停止しました".into())),
                    config_error: None,
                });
                *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(response);
            }
        });
        Self { sender, result }
    }
    pub fn request(&self, request: Request) -> Result<(), String> {
        self.sender.send(Some(request)).map_err(|_| "保存データの読み込み処理が停止しました".into())
    }
    pub fn take(&self) -> Option<Response> {
        self.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};
    struct CountingRoute(std::sync::atomic::AtomicUsize);
    impl tune_core::live::Route for CountingRoute {
        fn open(&self, _: &nodes::Node, _: &tune_core::live::Opts) -> Result<tune_core::live::Source, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("synthetic collector must not open".into())
        }
    }

    // Catches generic-panel fallback, missing range propagation, or any saved
    // route that mutates observations, metrics, cursors or computed health.
    #[test]
    fn saved_reader_reloads_all_routes_without_collection_or_writes() {
        let dir = std::env::temp_dir().join(format!("tune-dashboard-reader-{}-{}", std::process::id(), tune_core::db::now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let config = dir.join("nodes.json");
        std::fs::write(&config, json!({"nodes":[{"id":"example-node","os":"macos","alias":"unused","connectivity":false}]}).to_string()).unwrap();
        let engine = Engine::open(config, dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        let now = tune_core::db::now_ms();
        engine
            .with_db(|d| {
                d.add_metrics(&[tune_core::db::metrics::MinuteRow {
                    node_id: "example-node".into(),
                    minute: now - 60_000,
                    n: 60,
                    v: json!({"cpu":[20,30],"gpu_0_vram_used_mib":[1024,1100]}),
                    src: Some("example-hub".into()),
                }])?;
                d.set_meta("probeFailure:example-node", &json!({"at":now,"error":"synthetic failure"}))?;
                d.set_meta("hubPull", &json!({"at":now-3_600_000,"ok":false}))?;
                Ok(())
            })
            .unwrap();
        let saved = || {
            engine
                .with_db(|d| {
                    Ok(json!({
                        "metrics":d.metrics(None,0,i64::MAX)?.iter().map(|r| r.json()).collect::<Vec<_>>(),
                        "snapshots":d.history("example-node",20)?,"failure":d.get_meta("probeFailure:example-node")?,
                        "cursors":d.cursors()?,"checks":d.checks()?,"events":d.check_events(150)?,
                        "observations":d.monitor_events(&Default::default())?,"hub":d.get_meta("hubPull")?
                    }))
                })
                .unwrap()
        };
        let before = saved();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reader = Reader::new(engine.clone(), &rt);
        let route = Arc::new(CountingRoute(std::sync::atomic::AtomicUsize::new(0)));
        let live = tune_core::live::Live::new(route.clone(), Default::default(), Box::new(|_| {}));
        assert_eq!(Page::ALL.len(), 17);
        let route_reads = Page::ALL.len() * 2;
        for (i, page) in Page::ALL.into_iter().cycle().take(route_reads).enumerate() {
            let generation = i as u64 + 1;
            reader.request(Request { generation, route: Route::Page(page), query: String::new(), log_level: LogLevel::All, minutes: 360 }).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let response = loop {
                if let Some(r) = reader.take() {
                    break r;
                }
                assert!(Instant::now() < deadline, "Reader timed out");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(response.generation, generation);
            assert!(response.config_error.is_none());
            assert!(response.nodes.is_ok());
            if page == Page::Dashboard {
                let view = response.dashboard.expect("Dashboard needs a dedicated Reader result").unwrap();
                assert!(response.panel.is_none());
                let _rendered = crate::dashboard::render_dashboard(&view, &live, now);
                assert_eq!(view.saved["minutes"], 360);
                assert_eq!(view.nodes[0].saved["latest"]["src"], "example-hub");
            } else if page != Page::Overview {
                assert!(response.panel.unwrap().is_ok());
            }
        }
        assert_eq!(route.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(live.active().is_empty());
        assert_eq!(saved(), before);
        assert_eq!(engine.monitor_journal_health()["running"], false);
        assert_eq!(engine.monitor_journal_health()["status"], "unknown");
        assert!(!engine.is_probing());
        // A rejected current ledger must reach the dedicated Dashboard error
        // result too, rather than falling back to a generic record panel.
        std::fs::write(engine.config_path(), "invalid synthetic ledger").unwrap();
        let rejected_generation = route_reads as u64 + 1;
        reader
            .request(Request {
                generation: rejected_generation,
                route: Route::Page(Page::Dashboard),
                query: String::new(),
                log_level: LogLevel::All,
                minutes: 60,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let rejected = loop {
            if let Some(r) = reader.take() {
                break r;
            }
            assert!(Instant::now() < deadline, "Reader timed out on invalid ledger");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(rejected.generation, rejected_generation);
        assert!(rejected.config_error.is_some());
        assert!(rejected.dashboard.unwrap().is_err());
        assert!(rejected.panel.is_none(), "Dashboard errors belong to the dedicated result");
        drop(reader);
        drop(rt);
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
