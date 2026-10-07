// 画面から呼べるコマンドの一覧。ここに書いたものだけに allow-* の権限が作られ、capabilities/default.json で許可したものだけが呼べる
const COMMANDS: &[&str] = &[
    "config",
    "last",
    "probe",
    "history",
    "fleet",
    "actions_log",
    "logs_sync",
    "logs_query",
    "logs_signatures",
    "logs_cursors",
    "copy",
    "open_data_dir",
    "open_config",
    "status",
    "set_schedule",
    "dev_report",
];

fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS))).expect("tauri-build");
}
