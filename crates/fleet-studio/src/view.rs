use std::time::Duration;

use fleet_studio::Observation;
use fleet_studio::audit::{self, Audit, RepoAudit, Scope};
use fleet_studio::llm::{self, Endpoint, ModelError, ModelObservation};
use fleet_studio::navigation;
use fleet_studio::recent::{self, RecentEntry, RecentSlot, RecentWork, SyncStatus};
use fleet_studio::source::{self, Coverage, Dashboard, NodeCard, Repository};
use gpui::{Context, FocusHandle, IntoElement, KeyDownEvent, Render, SharedString, Window, div, prelude::*, rgb};

#[derive(Clone)]
struct ModelLine {
    label: String,
    endpoint: Option<Endpoint>,
    observation: ModelObservation,
}

pub struct FleetView {
    focus: FocusHandle,
    sample: bool,
    recent_local: bool,
    generation: u64,
    loading: bool,
    data_inflight: bool,
    load_timeout: bool,
    dashboard: Option<Dashboard>,
    models: Vec<ModelLine>,
    recent: RecentSlot,
    recent_open_error: Option<&'static str>,
    audit: Option<Audit>,
    audit_error: Option<String>,
    auditing: bool,
    audit_seq: u64,
    topology_searching: bool,
    topology_pending: bool,
    topology_seq: u64,
    search_error: Option<String>,
    topology_filter: String,
    search: String,
    searching: bool,
    selected: navigation::Selection,
}

impl FleetView {
    pub fn new(sample: bool, recent_local: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut view = Self {
            focus,
            sample,
            recent_local,
            generation: 0,
            loading: false,
            data_inflight: false,
            load_timeout: false,
            dashboard: None,
            models: Vec::new(),
            recent: RecentSlot::default(),
            recent_open_error: None,
            audit: None,
            audit_error: None,
            auditing: false,
            audit_seq: 0,
            topology_searching: false,
            topology_pending: false,
            topology_seq: 0,
            search_error: None,
            topology_filter: String::new(),
            search: String::new(),
            searching: false,
            selected: navigation::Selection::default(),
        };
        let executor = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            loop {
                executor.timer(Duration::from_secs(30)).await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.selected.invalidate();
        self.recent_open_error = None;
        if !self.sample {
            if let Some(generation) = self.recent.request() {
                self.start_recent(generation, cx);
            }
            cx.notify();
        }
        if self.data_inflight || self.models.iter().any(|line| line.endpoint.is_some() && line.observation.latest.is_none()) {
            return;
        }
        if let Some(audit) = &mut self.audit {
            for repo in &mut audit.repos {
                repo.previous = true;
            }
            for issue in &mut audit.issues {
                issue.previous = true;
            }
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        if self.sample {
            self.dashboard = Some(sample_dashboard());
            let mut observation = ModelObservation::default();
            observation.record(Ok(vec!["fixture-model".into()]), tune_core::db::now_ms());
            self.models = vec![ModelLine { label: "サンプル LLM".into(), endpoint: None, observation }];
            self.recent.data = Some(sample_recent());
            self.loading = false;
            cx.notify();
            return;
        }
        self.loading = true;
        self.data_inflight = true;
        self.load_timeout = false;
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.generation == generation && view.data_inflight {
                    view.loading = false;
                    view.load_timeout = true;
                    if let Some(data) = &mut view.dashboard {
                        for node in &mut data.nodes {
                            if matches!(node.observation, Observation::Current | Observation::Stale) {
                                node.observation = Observation::Unknown;
                            }
                        }
                        data.topology_coverage = Coverage::Unknown;
                        for repo in &mut data.repositories {
                            repo.previous = true;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        let task = cx
            .background_executor()
            .spawn(async move { source::load(&tune_core::nodes::user_config_path(), &tune_core::nodes::data_dir(), tune_core::db::now_ms()) });
        cx.spawn(async move |view, cx| {
            let mut data = task.await;
            let _ = view.update(cx, |view, cx| {
                if view.generation == generation {
                    if let Some(previous) = &view.dashboard {
                        source::preserve_previous(previous, &mut data);
                    }
                    view.dashboard = Some(data);
                    view.selected.invalidate();
                    view.topology_filter.clear();
                    view.loading = false;
                    view.data_inflight = false;
                    view.load_timeout = false;
                    if !view.search.is_empty() {
                        view.search_repositories(cx);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        let configured = std::env::var("FLEET_STUDIO_LLM_ENDPOINTS").ok();
        let (endpoints, rejected) = llm::configured(configured.as_deref());
        let previous = std::mem::take(&mut self.models);
        self.models = endpoints
            .iter()
            .map(|endpoint| {
                let mut observation =
                    previous.iter().find(|line| line.endpoint.as_ref() == Some(endpoint)).map(|line| line.observation.clone()).unwrap_or_default();
                observation.begin_refresh();
                ModelLine { label: endpoint.label.clone(), endpoint: Some(endpoint.clone()), observation }
            })
            .collect();
        if rejected > 0 {
            let mut observation = ModelObservation::default();
            observation.record(Err(ModelError::InvalidResponse), tune_core::db::now_ms());
            self.models.push(ModelLine { label: format!("LLM 接続設定 ({rejected} 件)"), endpoint: None, observation });
        }
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            let task = cx
                .background_executor()
                .spawn(async move { (llm::models(endpoint.address, endpoint.protocol, Duration::from_secs(2)), tune_core::db::now_ms()) });
            cx.spawn(async move |view, cx| {
                let (result, at) = task.await;
                let _ = view.update(cx, |view, cx| {
                    if view.generation == generation {
                        if let Some(line) = view.models.get_mut(index) {
                            line.observation.record(result, at);
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn start_recent(&mut self, generation: u64, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                view.recent.timeout(generation);
                cx.notify();
            });
        })
        .detach();
        let local_opt_in = self.recent_local;
        let task = cx
            .background_executor()
            .spawn(async move { recent::load(&tune_core::nodes::user_config_path(), &tune_core::nodes::data_dir(), tune_core::db::now_ms(), local_opt_in) });
        cx.spawn(async move |view, cx| {
            let data = task.await;
            let _ = view.update(cx, |view, cx| {
                if let Some(next) = view.recent.complete(generation, data) {
                    view.start_recent(next, cx);
                }
                view.selected.invalidate();
                cx.notify();
            });
        })
        .detach();
    }

    fn search_repositories(&mut self, cx: &mut Context<Self>) {
        if self.sample {
            return;
        }
        if self.topology_searching {
            self.topology_pending = true;
            return;
        }
        let query = self.search.clone();
        let generation = self.generation;
        self.topology_searching = true;
        self.topology_seq = self.topology_seq.wrapping_add(1);
        let seq = self.topology_seq;
        self.search_error = None;
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.topology_searching && view.topology_seq == seq {
                    view.search_error = Some("構成情報の検索が 5 秒を超過（終了待ち）".into());
                    if let Some(dashboard) = &mut view.dashboard {
                        dashboard.topology_coverage = Coverage::Unknown;
                        for repo in &mut dashboard.repositories {
                            repo.previous = true;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        let filter = query.clone();
        let task = cx.background_executor().spawn(async move { source::search_topology(&tune_core::nodes::data_dir(), &filter, tune_core::db::now_ms()) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.topology_searching = false;
                if view.generation == generation && view.search == query {
                    match result {
                        Ok(result) => {
                            if let Some(dashboard) = &mut view.dashboard {
                                dashboard.repositories = result.repositories;
                                dashboard.topology_coverage = result.coverage;
                                dashboard.topology_oldest_at = result.oldest_at;
                                dashboard.topology_limited = result.limited;
                                dashboard.topology_error = None;
                                view.search_error = None;
                                view.topology_filter = query.clone();
                                view.selected.invalidate();
                            }
                        }
                        Err(error) => {
                            if let Some(dashboard) = &mut view.dashboard {
                                dashboard.topology_coverage = Coverage::Unknown;
                                for repo in &mut dashboard.repositories {
                                    repo.previous = true;
                                }
                            }
                            view.search_error = Some(error.into());
                        }
                    }
                }
                if std::mem::take(&mut view.topology_pending) {
                    view.search_repositories(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn audit_repositories(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.auditing {
            return;
        }
        self.audit_error = None;
        if self.sample {
            self.audit = Some(Audit {
                repos: vec![RepoAudit {
                    node: "fixture-node".into(),
                    path: "fixture/project".into(),
                    branch: Some("feature".into()),
                    dirty: Some(1),
                    unpushed: Some(2),
                    behind: Some(0),
                    local_only: None,
                    previous: false,
                    local: false,
                    at: Some(tune_core::db::now_ms()),
                    error: None,
                }],
                incomplete_nodes: Vec::new(),
                issues: Vec::new(),
                observed_nodes: vec!["fixture-node".into()],
            });
            cx.notify();
            return;
        }
        self.auditing = true;
        self.audit_seq = self.audit_seq.wrapping_add(1);
        let seq = self.audit_seq;
        let generation = self.generation;
        let timer = cx.background_executor().timer(Duration::from_secs(65));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.auditing && view.audit_seq == seq {
                    view.audit_error = Some("監査が時間上限を超過（終了待ち）".into());
                    cx.notify();
                }
            });
        })
        .detach();
        let task = cx.background_executor().spawn(async move { audit::collect(&tune_core::nodes::user_config_path(), scope) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.auditing = false;
                if view.generation != generation {
                    view.audit_error = Some("台帳を再読込したため、進行中だった監査結果を破棄しました".into());
                } else {
                    match result {
                        Ok(mut audit) => {
                            if let Some(previous) = &view.audit {
                                audit::preserve_previous(previous, &mut audit);
                            }
                            view.audit = Some(audit);
                            view.audit_error = None;
                        }
                        Err(error) => {
                            if let Some(previous) = &mut view.audit {
                                for repo in &mut previous.repos {
                                    repo.previous = true;
                                }
                                for issue in &mut previous.issues {
                                    issue.previous = true;
                                }
                            }
                            view.audit_error = Some(error.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.searching {
            match key {
                "escape" => {
                    self.searching = false;
                    self.search_repositories(cx);
                }
                "enter" => {
                    self.searching = false;
                    self.search_repositories(cx);
                }
                "backspace" => {
                    self.search.pop();
                    self.selected.invalidate();
                }
                _ if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform => {
                    if let Some(text) = event.keystroke.key_char.as_deref().filter(|text| !text.chars().any(char::is_control)) {
                        if self.search.len().saturating_add(text.len()) <= 256 {
                            self.search.push_str(text);
                        }
                        self.selected.invalidate();
                    }
                }
                _ => {}
            }
        } else {
            match key {
                "/" | "slash" => self.searching = true,
                "r" => self.refresh(cx),
                "a" => self.audit_repositories(Scope::Local, cx),
                "down" => {
                    let count = self.repository_count()
                        + self.recent.data.as_ref().map_or(0, |data| data.entries.iter().filter(|entry| self.matches_recent(entry)).count());
                    self.selected.next(count);
                }
                "up" => {
                    let count = self.repository_count()
                        + self.recent.data.as_ref().map_or(0, |data| data.entries.iter().filter(|entry| self.matches_recent(entry)).count());
                    self.selected.previous(count);
                }
                "enter" => {
                    if self.selected.index().is_some_and(|index| index >= self.repository_count()) {
                        self.recent_open_error = Some("最近の作業の安全なリンク起動は未対応です。");
                    } else if let Some(url) = self.selected_url() {
                        cx.open_url(&url);
                    }
                }
                "escape" => {
                    self.search.clear();
                    self.selected.invalidate();
                    self.search_repositories(cx);
                }
                _ => {}
            }
        }
        self.focus.focus(window);
        cx.notify();
    }

    fn matches(&self, value: &str) -> bool {
        self.search.is_empty() || value.to_lowercase().contains(&self.search.to_lowercase())
    }

    fn matches_recent(&self, entry: &RecentEntry) -> bool {
        self.matches(&format!("{} {} {}", entry.node, entry.tool, entry.cwd.as_deref().unwrap_or("")))
    }

    fn repository_count(&self) -> usize {
        self.dashboard.as_ref().map_or(0, |dashboard| dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).take(200).count())
    }

    fn selected_url(&self) -> Option<String> {
        let selected = self.selected.index()?;
        let repo_count = self.repository_count();
        if selected < repo_count {
            return self
                .dashboard
                .as_ref()?
                .repositories
                .iter()
                .filter(|repo| self.matches(&repo.name))
                .take(200)
                .nth(selected)?
                .origin
                .as_deref()
                .and_then(navigation::repository_url);
        }
        None
    }
}

fn sample_recent() -> RecentWork {
    let now = tune_core::db::now_ms();
    RecentWork {
        nodes: vec![SyncStatus {
            node: "fixture-node".into(),
            attempt_at: Some(now),
            last_ok_at: Some(now),
            failure: Some(false),
            pending: Some(false),
            file_errors: Some(0),
            session_error: false,
        }],
        entries: vec![RecentEntry {
            node: "fixture-node".into(),
            local: true,
            tool: "fixture-agent".into(),
            cwd: Some("fixture/project".into()),
            activity_at: Some(now),
            directory_url: None,
            prs: vec!["https://github.com/example/fixture/pull/1".into()],
            pr_state: recent::PrState::Complete,
        }],
        max_age_ms: Some(90 * 60 * 1_000),
        validated_at: Some(now),
        ..Default::default()
    }
}

fn sample_dashboard() -> Dashboard {
    let now = tune_core::db::now_ms();
    Dashboard {
        nodes: vec![NodeCard {
            id: "fixture-node".into(),
            role: "research".into(),
            operating_state: "online".into(),
            local: Some(true),
            last_at: Some(now),
            observation: Observation::Current,
            failure_reason: None,
            max_age_ms: Some(180 * 60 * 1_000),
            score: Some(90),
            critical: Some(0),
            warnings: Some(1),
        }],
        config_error: None,
        database_error: None,
        repositories: vec![Repository {
            name: "fixture-repository".into(),
            origin: Some("https://github.com/example/fixture".into()),
            observed_at: Some(now),
            previous: false,
        }],
        topology_coverage: Coverage::Complete,
        topology_error: None,
        topology_oldest_at: Some(now),
        topology_limited: false,
    }
}

fn when(at: Option<i64>) -> String {
    at.and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "時刻不明".into())
}

fn observation_text(value: Observation) -> &'static str {
    match value {
        Observation::Current => "保存データが新しい",
        Observation::Stale => "古い",
        Observation::Failed => "取得失敗",
        Observation::Unknown => "不明",
        Observation::Excluded => "対象外",
    }
}

fn coverage_text(value: Coverage) -> &'static str {
    match value {
        Coverage::Complete => "取得済み",
        Coverage::Selected => "表示分のみ・全体は不明",
        Coverage::Partial => "一部・未確定",
        Coverage::Stale => "古い",
        Coverage::Unknown => "不明",
    }
}

fn model_text(line: &ModelLine) -> String {
    let text: String = match &line.observation.latest {
        None => "取得中".into(),
        Some(Ok(models)) if models.is_empty() => "応答あり・モデル一覧は空".into(),
        Some(Ok(models)) => models.join(", "),
        Some(Err(ModelError::TimedOut)) => "タイムアウト".into(),
        Some(Err(ModelError::Unavailable)) => "接続できません".into(),
        Some(Err(ModelError::InvalidResponse)) => "応答を解釈できません".into(),
        Some(Err(ModelError::Http(code))) => format!("HTTP {code}"),
    };
    if !matches!(&line.observation.latest, Some(Ok(_)))
        && let Some((models, at)) = &line.observation.last_good
    {
        let age = match line.observation.last_good_freshness(tune_core::db::now_ms()) {
            Observation::Stale => "（古い）",
            Observation::Unknown => "（時刻不明）",
            _ => "",
        };
        return format!("{text} / 前回{age}: {} ({})", if models.is_empty() { "一覧は空".into() } else { models.join(", ") }, when(Some(*at)));
    }
    if matches!(&line.observation.latest, Some(Ok(_))) {
        match fleet_studio::observation(line.observation.checked_at, false, true, tune_core::db::now_ms(), llm::MODEL_MAX_AGE_MS) {
            Observation::Stale => return format!("古い / {text}"),
            Observation::Unknown => return format!("時刻不明 / {text}"),
            _ => {}
        }
    }
    text
}

fn number(value: Option<u64>) -> String {
    value.map_or_else(|| "不明".into(), |value| value.to_string())
}

fn recent_status(status: &SyncStatus, now: i64, max_age_ms: Option<i64>) -> String {
    let latest = match status.collection_state() {
        recent::CollectionState::Failed => "失敗",
        recent::CollectionState::Succeeded => "保存上は成功",
        recent::CollectionState::Unknown => "不明",
    };
    let (pending, partial) = if status.last_ok_at.is_some() {
        (
            match status.pending {
                Some(true) => "あり",
                Some(false) => "なし",
                None => "不明",
            },
            recent::file_error_count_text(status.file_errors),
        )
    } else {
        ("不明", "不明".into())
    };
    format!(
        "最新取込の記録: {latest} {} / 最終成功の記録: {}（{}） / 続き・再読込: {pending} / 一部失敗: {partial}{}",
        when(status.attempt_at),
        when(status.last_ok_at),
        observation_text(status.freshness(now, max_age_ms.unwrap_or(-1))),
        if status.session_error { " / 作業一覧の読取失敗" } else { "" },
    )
}

impl Render for FleetView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = tune_core::db::now_ms();
        let mut content = div().id("content").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5();
        if self.load_timeout {
            content = content.child(note("保存データの読込が 5 秒を超過（終了待ち）。前回の結果は不明として表示します。".into(), 0xffb86c));
        }
        if let Some(dashboard) = &self.dashboard {
            if let Some(error) = &dashboard.config_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if let Some(error) = &dashboard.database_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            content = content.child(section("機体", if self.loading { "読み込み中" } else { "保存済み分析（現在の接続状態ではありません）" }));
            for node in dashboard.nodes.iter().filter(|node| self.matches(&format!("{} {} {}", node.id, node.role, node.operating_state))) {
                let identity = node.identity_text();
                content = content.child(row(
                    format!("{}  ·  {}", node.id, node.role),
                    format!(
                        "{}  /  台帳: {}  /  {}  /  重大 {}・注意 {}・score {}  /  最終成功: {}{}",
                        observation_text(node.current_observation(now)),
                        node.operating_state,
                        identity,
                        number(node.critical),
                        number(node.warnings),
                        node.score.map_or_else(|| "不明".into(), |value| value.to_string()),
                        when(node.last_at),
                        node.failure_reason.map_or(String::new(), |reason| format!("  /  理由: {reason}"))
                    ),
                ));
            }
            content = content.child(section("リポジトリ", &format!("構成情報: {}", coverage_text(dashboard.current_coverage(now)))));
            if let Some(error) = &dashboard.topology_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if let Some(error) = &self.search_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if self.topology_searching {
                content = content.child(note("保存済みの構成情報を検索中…".into(), 0x9baec4));
            }
            if dashboard.topology_limited {
                content = content.child(note("構成情報には取得上限または未確定候補があります。保存 DB の構成情報の検索は未対応です。".into(), 0xffb86c));
            }
            let search_unapplied = !self.sample && self.search != self.topology_filter;
            if search_unapplied {
                content = content.child(note("表示範囲を検索しています。保存 DB の構成情報の検索は未対応です。".into(), 0xffb86c));
            }
            let mut shown = 0;
            for (index, repo) in dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).take(200).enumerate() {
                shown += 1;
                let url = repo.origin.as_deref().and_then(navigation::repository_url);
                let mut item = div()
                    .id(SharedString::from(format!("repo-{index}")))
                    .p_3()
                    .rounded_md()
                    .bg(rgb(if self.selected.index() == Some(index) { 0x233b56 } else { 0x182436 }))
                    .flex()
                    .flex_col()
                    .child(format!("{}{}", if self.selected.index() == Some(index) { "› " } else { "  " }, repo.name))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(format!(
                        "{} / 取得: {}",
                        if repo.previous { "前回（取得失敗・未確認）" } else { observation_text(repo.freshness(now)) },
                        when(repo.observed_at)
                    )));
                if let Some(url) = url {
                    item = item.cursor_pointer().on_click(move |_, _, cx| cx.open_url(&url));
                }
                content = content.child(item);
            }
            let total = dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).count();
            if total > shown {
                content = content.child(note(format!("{total} 件中 {shown} 件を表示。検索で絞り込めます。"), 0x9baec4));
            }
            if shown == 0 {
                let has_error = dashboard.topology_error.is_some() || self.search_error.is_some();
                content = content.child(note(source::empty_repository_text(dashboard.current_coverage(now), has_error).into(), 0x9baec4));
            }
        } else {
            content = content.child(note("保存済みデータを読み込み中…".into(), 0x9baec4));
        }

        content = content.child(section("最近の作業", "保存済み・現在の稼働状態ではありません"));
        content = content
            .child(note("保存された取込上の記録です。収集の正常完了や網羅性は保証できず、台帳の機体 ID は接続先の本人確認ではありません。".into(), 0x9baec4));
        content = content.child(note(
            "既定は非表示。明示して起動した場合だけこの機体の親セッションを最大 20 件表示します。遠隔機は対象外です。検索は表示範囲の機体・ツール・作業場所です。".into(),
            0x9baec4,
        ));
        if self.recent.timed_out {
            content = content.child(note("最近の作業の読込が 5 秒を超過（終了待ち）。以前の内容は非表示です。".into(), 0xffb86c));
        } else if self.recent.in_flight.is_some() {
            content = content.child(note("最近の作業を読み込み中…".into(), 0x9baec4));
        }
        if let Some(recent) = &self.recent.data {
            content = content.child(note(format!("台帳照合: {}。以降の変更は自動反映されません。R で更新してください。", when(recent.validated_at)), 0x9baec4));
            if let Some(error) = recent.error {
                content = content.child(note(error.into(), 0xffb86c));
            }
            if let Some(error) = self.recent_open_error {
                content = content.child(note(error.into(), 0xffb86c));
            }
            if recent.node_limited || recent.entries_limited || recent.selection_incomplete {
                content = content.child(note("対象または表示範囲に上限・未確認があります。表示外は未確認です。".into(), 0xffb86c));
            }
            if recent.local_ambiguous {
                content = content.child(note("この機体の照合が複数一致したため不明です。一致した機体の作業は表示しません。".into(), 0xffb86c));
            }
            for status in recent.nodes.iter().filter(|status| self.matches(&status.node)) {
                content = content.child(row(status.node.clone(), recent_status(status, now, recent.max_age_ms)));
            }
            let repo_count = self.repository_count();
            for (index, entry) in recent.entries.iter().filter(|entry| self.matches_recent(entry)).enumerate() {
                let collection = recent
                    .nodes
                    .iter()
                    .find(|status| status.node == entry.node)
                    .map(|status| recent_status(status, now, recent.max_age_ms))
                    .unwrap_or_else(|| "収集状態: 不明".into());
                let prs = match entry.pr_state {
                    recent::PrState::Unknown => "PR: 不明".to_owned(),
                    recent::PrState::Empty => "PR: なし".to_owned(),
                    recent::PrState::Complete => format!("PR: {} 件（安全な起動は未対応）", entry.prs.len()),
                    recent::PrState::Partial => "PR: 一部未確認または上限（安全な起動は未対応）".to_owned(),
                };
                content = content.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(if self.selected.index() == Some(repo_count + index) { 0x233b56 } else { 0x182436 }))
                        .flex()
                        .flex_col()
                        .child(format!("{}{} · {}", if self.selected.index() == Some(repo_count + index) { "› " } else { "  " }, entry.node, entry.tool))
                        .child(div().text_sm().text_color(rgb(0x9baec4)).child(format!(
                            "作業場所: {}{} / 最終活動: {}",
                            entry.cwd.as_deref().unwrap_or("不明"),
                            if entry.local { "" } else { "（遠隔・ローカルでは開けません）" },
                            when(entry.activity_at)
                        )))
                        .child(div().text_sm().text_color(rgb(0x9baec4)).child(collection))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x9baec4))
                                .child(format!("{prs}{}", if entry.directory_url.is_some() { " / 作業場所の安全な起動は未対応" } else { "" })),
                        ),
                );
            }
            if recent.entries.is_empty() && recent.error.is_none() {
                content = content.child(note("許可された機体に表示できる保存済み作業はありません。取得状態は上を確認してください。".into(), 0x9baec4));
            }
        }

        content = content.child(section("ローカル LLM", "モデル一覧のみ取得・推論なし"));
        for model in self.models.iter().filter(|model| self.matches(&model.label) || self.matches(&model_text(model))) {
            content = content.child(row(model.label.clone(), format!("{}  /  今回: {}", model_text(model), when(model.observation.checked_at))));
        }
        content = content.child(section("リポジトリ衛生", if self.auditing { "取得中…" } else { "この機体のみ A / 遠隔は権限・接続先未確認のため未対応" }));
        content =
            content.child(note("最後に fetch した追跡参照との差です。探索先の一部欠落やアクセス拒否は元ツールで判別できない場合があります。".into(), 0x9baec4));
        if let Some(error) = &self.audit_error {
            content = content.child(note(error.clone(), 0xffb86c));
        }
        if let Some(audit) = &self.audit {
            for issue in &audit.issues {
                content = content
                    .child(note(format!("{}: {}{} / {}", issue.node, if issue.previous { "前回: " } else { "" }, issue.reason, when(issue.at)), 0xffb86c));
            }
            for (index, repo) in audit.repos.iter().filter(|repo| self.matches(&format!("{} {}", repo.node, repo.path))).take(200).enumerate() {
                let detail = format!(
                    "{}  /  変更・未追跡 {}  /  未 push {}  /  behind {}  /  remote に無い {}  /  {}  /  {}{}",
                    repo.branch.as_deref().unwrap_or("branch 不明"),
                    number(repo.dirty),
                    number(repo.unpushed),
                    number(repo.behind),
                    number(repo.local_only),
                    if repo.previous {
                        "前回"
                    } else {
                        observation_text(fleet_studio::observation(repo.at, repo.error.is_some(), true, now, 60 * 60 * 1_000))
                    },
                    when(repo.at),
                    repo.error.map_or(String::new(), |error| format!(" / {error}"))
                );
                let local_url = repo.local.then(|| navigation::local_directory(&repo.path)).flatten();
                let mut item = div()
                    .id(SharedString::from(format!("audit-{index}")))
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x182436))
                    .flex()
                    .flex_col()
                    .child(format!("{}{} · {}", repo.node, if repo.local { "（この機体）" } else { "（台帳名・接続先 hostname 未照合）" }, repo.path))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(detail));
                if let Some(url) = local_url {
                    item = item.cursor_pointer().on_click(move |_, _, cx| cx.open_url(&url));
                }
                content = content.child(item);
            }
            let total = audit.repos.iter().filter(|repo| self.matches(&format!("{} {}", repo.node, repo.path))).count();
            if total > 200 {
                content = content.child(note(format!("{total} 件中 200 件を表示。検索で絞り込めます。"), 0x9baec4));
            }
        } else if !self.auditing {
            content = content.child(note("未実行。未確認のリポジトリは正常と数えません。".into(), 0x9baec4));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0d1725))
            .text_color(rgb(0xe7edf6))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .px_5()
                    .py_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(0x132237))
                    .child(div().flex().flex_col().child(div().text_xl().child("Fleet Studio")).child(
                        div().text_sm().text_color(rgb(0x9baec4)).child(if self.sample {
                            "サンプルデータ · 読み取り専用"
                        } else {
                            "KatalaTune のデータ · 読み取り専用"
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("refresh")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("更新 R")
                                    .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                            )
                            .child(
                                div()
                                    .id("audit")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("この機体を監査 A")
                                    .on_click(cx.listener(|view, _, _, cx| view.audit_repositories(Scope::Local, cx))),
                            )
                            .child(
                                div()
                                    .id("audit-fleet")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("遠隔監査（未対応）")
                                    .on_click(cx.listener(|view, _, _, cx| view.audit_repositories(Scope::Fleet, cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .id("search")
                    .px_5()
                    .py_3()
                    .bg(rgb(0x192b42))
                    .cursor_pointer()
                    .child(format!(
                        "検索 / : {}{}",
                        if self.search.is_empty() { "機体・リポジトリ・モデル" } else { &self.search },
                        if self.searching { " ▏" } else { "" }
                    ))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.searching = true;
                        view.focus.focus(window);
                        cx.notify();
                    })),
            )
            .child(content)
            .child(
                div()
                    .px_5()
                    .py_3()
                    .flex()
                    .justify_between()
                    .bg(rgb(0x132237))
                    .child("/ 検索（Enter で確定） · ↑↓ 選択 · Enter で開く · 最近の作業の起動は未対応")
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(
                                div()
                                    .id("documentation")
                                    .cursor_pointer()
                                    .child("説明")
                                    .on_click(|_, _, cx| cx.open_url("https://github.com/katalalab/katala-tune#readme")),
                            )
                            .child(
                                div()
                                    .id("issues")
                                    .cursor_pointer()
                                    .child("Issues")
                                    .on_click(|_, _, cx| cx.open_url("https://github.com/katalalab/katala-tune/issues")),
                            ),
                    ),
            )
    }
}

fn section(title: &'static str, subtitle: &str) -> impl IntoElement {
    div()
        .pt_3()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_lg().child(title))
        .child(div().text_sm().text_color(rgb(0x9baec4)).child(subtitle.to_owned()))
}

fn row(title: String, detail: String) -> impl IntoElement {
    div().p_3().rounded_md().bg(rgb(0x182436)).flex().flex_col().child(title).child(div().text_sm().text_color(rgb(0x9baec4)).child(detail))
}

fn note(message: String, color: u32) -> impl IntoElement {
    div().p_2().text_sm().text_color(rgb(color)).child(message)
}
