// リリースの Windows ではコンソール窓を出さない
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    katala_tune::run();
}
