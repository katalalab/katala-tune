//! One background reader per window. The watch mailbox coalesces navigation
//! requests; a slow query cannot create an unbounded queue of database readers.
use crate::{
    model::{self, NodeView},
    panels::{self, Page, Panel},
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
}
pub struct Response {
    pub generation: u64,
    pub nodes: Result<Vec<NodeView>, String>,
    pub panel: Option<Result<Panel, String>>,
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
                let response = tokio::task::spawn_blocking(move || {
                    let cfg = engine.reload_config();
                    if let Some(error) = engine.config_error() {
                        return Response { generation, nodes: Err(error.clone()), panel: Some(Err(error.clone())), config_error: Some(error) };
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
                    let panel = match request.route {
                        Route::Page(page) if page != Page::Overview => Some(panels::load(&engine, page, &request.query)),
                        _ => None,
                    };
                    Response { generation, nodes, panel, config_error: engine.config_error() }
                })
                .await
                .unwrap_or_else(|_| Response {
                    generation,
                    nodes: Err("保存データの読み込みタスクが停止しました".into()),
                    panel: None,
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
