//! Static CLI discovery. Never open user state or start collectors here.
use serde_json::{Value, json};
use std::process::ExitCode;

const CONFIG: u16 = 1;
const DB: u16 = 2;
const WRITE_CONFIG: u16 = 4;
const WRITE_DB: u16 = 8;
const CONNECT: u16 = 16;
const DEPLOY: u16 = 32;
const LINK: u16 = 64;
const WRITE_LINK: u16 = 128;
// Engine::open may install example config, initialize/migrate DB and import legacy logs.
const ENGINE: u16 = CONFIG | DB | WRITE_CONFIG | WRITE_DB;

struct Command {
    name: &'static str,
    arguments: &'static [&'static str],
    summary: &'static str,
    format: &'static str,
    effects: u16,
    notes: &'static str,
}

// Argument strings are usage syntax: [] optional, <> required, ... repeatable.
// Effects describe possibilities across options/platforms, not a permission grant.
const COMMANDS: &[Command] = &[
    Command {
        name: "version",
        arguments: &["[--json]"],
        summary: "CLI の版とビルド情報を表示",
        format: "text",
        effects: 0,
        notes: "--version / -V はテキスト表示。--json は機体監査向けの版・schema・実行ファイル指紋。設定・DB・接続を初期化しない。",
    },
    Command {
        name: "versions",
        arguments: &["[node ...]"],
        summary: "機体ごとの CLI の版と指紋を照合",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時は稼働中の収集許可機。共用機は specs:true の明示指定が必要。読取専用・DB保存なし。不一致・取得失敗時はJSONを返して終了コード1。",
    },
    Command {
        name: "specs-local",
        arguments: &[],
        summary: "この機体の部品・版・構成・設定を収集",
        format: "json",
        effects: 0,
        notes: "katala_specs.v1。ローカルのOS内蔵ツールを読む。台帳・DB不要。追加ソフトは導入しない。",
    },
    Command {
        name: "specs",
        arguments: &["[--saved]", "[node ...]"],
        summary: "機体仕様を収集・保存し比較用 JSON を表示",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "省略時は稼働中の収集許可機。共用機は specs:true の明示指定が必要。--saved は既存DBだけを読取専用で開き、保存済み仕様・最終試行を表示。接続・作成・移行なし。保存済み表示では退役・収集対象外の機体も指定できる。",
    },
    Command {
        name: "help",
        arguments: &["[command]"],
        summary: "CLI の使い方と副作用を表示",
        format: "text",
        effects: 0,
        notes: "--help は全体ヘルプの別名。設定・DB・接続を初期化しない。",
    },
    Command {
        name: "cli-info",
        arguments: &["--json"],
        summary: "CLI 自身のコマンド一覧を JSON で表示",
        format: "json",
        effects: 0,
        notes: "設定・DB・接続を初期化しない。機体 firmware の調査は capabilities。",
    },
    Command {
        name: "paths",
        arguments: &[],
        summary: "台帳と DB の場所を表示",
        format: "json",
        effects: 0,
        notes: "ファイルは開かない。環境に依存するパス・機体名を含む。",
    },
    Command {
        name: "probe",
        arguments: &["[node ...]"],
        summary: "機体を調査し結果を表示",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時は全運用対象。DBへ保存しない。機体ごとの失敗は結果内に含む。",
    },
    Command {
        name: "logs",
        arguments: &["[--db <dir>]", "[node ...]"],
        summary: "ログを DB に取り込む",
        format: "none",
        effects: CONFIG | DB | WRITE_DB | CONNECT,
        notes: "省略時は全運用対象。DB既定は一時ディレクトリの katala-tune-cli。進捗・結果は stderr。",
    },
    Command {
        name: "ledger",
        arguments: &["[--json]"],
        summary: "対象台帳と既存の観測状況を表示",
        format: "text",
        effects: CONFIG | DB,
        notes: "--json で JSON。台帳・DBの作成や移行、機体接続はしない。",
    },
    Command {
        name: "last",
        arguments: &[],
        summary: "前回の分析結果を表示",
        format: "json",
        effects: ENGINE,
        notes: "照会でも Engine 初期化による書込があり得る。",
    },
    Command {
        name: "power",
        arguments: &[],
        summary: "電力・Clock・計算条件を表示",
        format: "json",
        effects: ENGINE,
        notes: "照会でも Engine 初期化による書込があり得る。",
    },
    Command {
        name: "capabilities",
        arguments: &["[node ...]"],
        summary: "機体の firmware 能力を調査",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時はこの機体。保存なし。CLI自身の一覧は cli-info。",
    },
    Command {
        name: "maintenance-plan",
        arguments: &["[node ...]"],
        summary: "firmware 読取と実行不可の保守計画を表示",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時はこの機体。変更操作は実行しない。",
    },
    Command {
        name: "network",
        arguments: &["[--probe]", "[node ...]"],
        summary: "軽量な接続診断を表示",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時はこの機体。--probe でHTTPS到達性・時間も測る。DB保存なし。",
    },
    Command {
        name: "status",
        arguments: &["[--recompute]"],
        summary: "保存済み状態または再計算した状態を表示",
        format: "json",
        effects: ENGINE,
        notes: "--recompute は計算結果を保存する。省略しても Engine 初期化による書込があり得る。",
    },
    Command {
        name: "analyze",
        arguments: &["<snapshot.json>"],
        summary: "snapshot ファイルから所見とスコアを作る",
        format: "json",
        effects: 0,
        notes: "指定ファイルを読む。台帳・DBは開かない。",
    },
    Command {
        name: "inventory",
        arguments: &["[node ...]"],
        summary: "道具の棚卸しを DB に保存",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "省略時は全運用対象。",
    },
    Command {
        name: "ai",
        arguments: &["[node ...]"],
        summary: "AI セッションを DB に取り込む",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "省略時は全運用対象。続きの位置をDBに保存する。",
    },
    Command {
        name: "ai-summary",
        arguments: &["[days [node]]"],
        summary: "AI セッションの量・費用の集計を表示",
        format: "json",
        effects: ENGINE,
        notes: "既定30日、全機体。照会でも Engine 初期化による書込があり得る。",
    },
    Command {
        name: "ai-provenance",
        arguments: &[],
        summary: "AI データの出どころと検算状況を表示",
        format: "json",
        effects: ENGINE,
        notes: "照会でも Engine 初期化による書込があり得る。",
    },
    Command {
        name: "ai-trace",
        arguments: &["<node>", "<file-key|day-epoch-ms>"],
        summary: "AI の数字の出どころを表示",
        format: "json",
        effects: ENGINE,
        notes: "ファイル区間または1日の内訳を照会する。",
    },
    Command {
        name: "ai-verify",
        arguments: &["<node>", "<file-key>"],
        summary: "AI 区間を機体上の元ファイルと照合",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "機体では読み直して指紋を取得。ローカルDBに照合結果を保存し、Engine初期化でも書込があり得る。",
    },
    Command {
        name: "ai-limits",
        arguments: &["[node ...]"],
        summary: "Codex の残り枠を問い合わせる",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "省略時は全運用対象。共用機は対象外。app-serverを呼び、取得間隔の下限は見ない。",
    },
    Command {
        name: "netsec",
        arguments: &["[--probe]", "[node ...]"],
        summary: "ネットワークとセキュリティの状態を表示",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "--probe で分析・保存。省略時もチェックを計算する。設定変更や遮断は行わない。",
    },
    Command {
        name: "hygiene",
        arguments: &["[--ghq-root <dir>]", "[--root <dir>]...", "[--max-repos <n>]", "[--budget <seconds>]", "[--repos]", "[node ...]"],
        summary: "リポジトリの衛生状態を監査",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "省略時は全運用対象のうち有効な機体。opt-in・読取専用・DB保存なし。--repos で個別行も出す。",
    },
    Command {
        name: "export",
        arguments: &["[--since <epoch-ms>]"],
        summary: "常時監視の集計と最新結果をエクスポート",
        format: "json",
        effects: DB | WRITE_DB,
        notes: "Store::open によるDB作成・移行があり得る。旧ログimportはしない。",
    },
    Command {
        name: "hub-pull",
        arguments: &[],
        summary: "ハブから監視データを1回取り込む",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "台帳のハブへSSHし tune export を呼ぶ。相手側でもDB初期化があり得る。",
    },
    Command {
        name: "monitor",
        arguments: &["[settings-json]"],
        summary: "常時監視とハブの設定を表示・更新",
        format: "json",
        effects: ENGINE,
        notes: "JSON例: {\"enabled\":true,\"hub\":\"id\"}。設定はDB保存。省略時もEngine初期化あり。",
    },
    Command {
        name: "schedule",
        arguments: &["[settings-json]"],
        summary: "自動スキャンの間隔を表示・更新",
        format: "json",
        effects: ENGINE,
        notes: "JSON例: {\"logs_minutes\":60}。設定はDB保存。省略時もEngine初期化あり。",
    },
    Command {
        name: "metrics",
        arguments: &["[node]", "[--minutes <n>]"],
        summary: "分ごとの集計を表示",
        format: "json",
        effects: ENGINE,
        notes: "既定60分、全機体。照会でもEngine初期化あり。",
    },
    Command {
        name: "dashboard",
        arguments: &["[--minutes <n>]"],
        summary: "ダッシュボードの保存済み集計を表示",
        format: "json",
        effects: ENGINE,
        notes: "既定60分。ライブの段階は含まない。照会でもEngine初期化あり。",
    },
    Command {
        name: "update",
        arguments: &["[settings-json]"],
        summary: "自動更新の設定と状態を表示・更新",
        format: "json",
        effects: ENGINE,
        notes: "JSON例: {\"auto\":false}。設定はDB保存。このコマンド自体は更新配布を実行しない。",
    },
    Command {
        name: "monitor-run",
        arguments: &["[--seconds <n>]"],
        summary: "画面なしで有限時間の常時監視を実行",
        format: "json",
        effects: ENGINE | CONNECT | DEPLOY,
        notes: "既定180秒、15〜3600秒。Windowsでは同梱samplerを古い機体へ配布する可能性あり。",
    },
    Command {
        name: "live",
        arguments: &["[--seconds <n>]", "<node ...>"],
        summary: "ライブ sampler の届き方を計測",
        format: "json",
        effects: CONFIG | CONNECT,
        notes: "対象必須。既定30秒、1〜300秒。DB保存なし。",
    },
    Command {
        name: "agent-pair",
        arguments: &["<host[:port]>"],
        summary: "tune-agent とペアリング",
        format: "json",
        effects: LINK | WRITE_LINK | CONNECT,
        notes: "コードは標準入力。鍵とpeer台帳を作成・更新する。",
    },
    Command {
        name: "agent-peers",
        arguments: &[],
        summary: "ペア済み tune-agent を表示",
        format: "json",
        effects: LINK,
        notes: "既存鍵・peer台帳を読む。新規作成しない。",
    },
    Command {
        name: "agent-probe",
        arguments: &["<name|fingerprint|addr>"],
        summary: "ペア済み tune-agent を1回調査",
        format: "json",
        effects: LINK | CONNECT,
        notes: "SSHを使わない。機体ごとの失敗はJSON内に含む。",
    },
    Command {
        name: "agent-unpair",
        arguments: &["<name|fingerprint>"],
        summary: "peer 台帳からペアを削除",
        format: "json",
        effects: LINK | WRITE_LINK,
        notes: "一致する1件を削除する。",
    },
    Command {
        name: "topology-refresh",
        arguments: &["[node ...]"],
        summary: "許可された topology を収集・保存",
        format: "json",
        effects: ENGINE | CONNECT,
        notes: "省略時は設定済み対象。opt-inと収集上限に従う。",
    },
    Command {
        name: "topology-watch",
        arguments: &["[--iterations <n>]", "[--interval-seconds <n>]", "[node ...]"],
        summary: "有限回数 topology を収集・保存",
        format: "json-lines",
        effects: ENGINE | CONNECT,
        notes: "既定1回、1〜100回。間隔は既定schedule、1〜86400秒。各回1行のJSON。",
    },
    Command {
        name: "topology-export",
        arguments: &["[--view <all|hosts|dependencies|cicd>]", "[--format <json|mermaid>]"],
        summary: "公開用 topology を表示",
        format: "json",
        effects: ENGINE,
        notes: "既定all/json。照会でも Engine 初期化による書込があり得る。",
    },
];

fn effects(bits: u16) -> Value {
    json!({
        "reads_config": bits & CONFIG != 0, "reads_db": bits & DB != 0,
        "writes_config": bits & WRITE_CONFIG != 0, "writes_db": bits & WRITE_DB != 0,
        "connects_to_nodes": bits & CONNECT != 0, "may_deploy_helper": bits & DEPLOY != 0,
        "reads_link_state": bits & LINK != 0, "writes_link_state": bits & WRITE_LINK != 0
    })
}

fn catalog() -> Value {
    json!({
        "schema_version": 1,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "specs_schema": tune_core::specs::SCHEMA,
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "effects_semantics": "Possible effects across options/platforms; initialization counts as writing. Not a sandbox or authorization policy. Collectors may run processes and use temporary files; connection tools may maintain their own state.",
        "argument_syntax": "[] optional, <> required, ... repeatable; descriptive usage, not an input-validation schema",
        "environment": ["KATALA_TUNE_CONFIG", "KATALA_TUNE_DATA_DIR", "KATALA_TUNE_LINK_DIR"],
        "commands": COMMANDS.iter().map(|c| json!({
            "name": c.name, "arguments": c.arguments, "summary": c.summary,
            "output": {"default": c.format, "formats": match c.name {
                "ledger" | "version" => vec!["text", "json"], "topology-export" => vec!["json", "mermaid"], _ => vec![c.format]
            }},
            "effects": effects(c.effects), "notes": c.notes
        })).collect::<Vec<_>>()
    })
}

fn help(command: Option<&Command>) {
    println!("Katala Tune {}\n使い方: tune <command> [arguments]\n", env!("CARGO_PKG_VERSION"));
    for c in COMMANDS.iter().filter(|c| command.is_none_or(|selected| c.name == selected.name)) {
        println!("  {} {}\n    {}", c.name, c.arguments.join(" "), c.summary);
        if command.is_some() {
            println!("    {}\n    stdout: {}\n    effects: {}", c.notes, c.format, effects(c.effects));
        }
    }
    println!("\ntune help <command> で詳細、tune cli-info --json で機械向け一覧。");
    println!("KATALA_TUNE_CONFIG=台帳、KATALA_TUNE_DATA_DIR=DBディレクトリ、KATALA_TUNE_LINK_DIR=鍵・peer台帳。logs のDBは --db で指定。");
    println!("副作用は実行条件による可能性。読取系もDB初期化・移行を行う場合がある。変更操作(actions)はアプリの確認ダイアログからだけ実行。");
}

/// Only intercept new discovery entries; existing dispatch and native protocols stay intact.
pub fn run(args: &[String]) -> Option<ExitCode> {
    let cmd = args.first()?.as_str();
    match (cmd, &args[1..]) {
        ("--help" | "-h" | "help", []) => help(None),
        ("--version" | "-V" | "version", []) => println!("tune {}", env!("CARGO_PKG_VERSION")),
        ("version", [option]) if option == "--json" => {
            println!("{}", tune_core::version_inventory::local());
        }
        (name, [option]) if option == "--help" || option == "-h" => match COMMANDS.iter().find(|c| c.name == name) {
            Some(c) => help(Some(c)),
            None => return None,
        },
        ("help", [name]) => match COMMANDS.iter().find(|c| c.name == name) {
            Some(c) => help(Some(c)),
            None => {
                eprintln!("Unknown help command: {name}");
                return Some(ExitCode::from(2));
            }
        },
        ("cli-info", [option]) if option == "--json" => println!("{}", catalog()),
        ("--help" | "-h" | "help" | "cli-info" | "--version" | "-V" | "version", _) => {
            eprintln!("使い方: tune --help | tune help [command] | tune cli-info --json | tune version [--json]");
            return Some(ExitCode::from(2));
        }
        _ => return None,
    }
    Some(ExitCode::SUCCESS)
}
