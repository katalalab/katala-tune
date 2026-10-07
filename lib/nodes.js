// 機体台帳を読み、この機体をローカル実行に切り替える。
// 台帳は個人の情報なのでリポジトリに置かず ~/.config/katala-tune/nodes.json を使う（見本は config/nodes.example.json）
'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const expand = (p) => p.replace(/^~(?=[\\/]|$)/, os.homedir());
const USER_CONFIG = path.join(os.homedir(), '.config', 'katala-tune', 'nodes.json');
const EXAMPLE = path.join(__dirname, '..', 'config', 'nodes.example.json');

// 読めなければ throw する（保護リストを読めないまま操作しないため、呼び出し側は失敗を握りつぶさない）
function loadConfig(file = USER_CONFIG) {
  const cfg = JSON.parse(fs.readFileSync(file, 'utf8'));
  if (!Array.isArray(cfg.nodes)) throw new Error(`${file}: nodes が配列ではない`);
  if (cfg.protect != null && !Array.isArray(cfg.protect)) throw new Error(`${file}: protect が配列ではない`);
  const host = os.hostname().replace(/\.local$/, '').toLowerCase();
  cfg.protect = (cfg.protect || []).map(String);
  cfg.nodes = cfg.nodes.map((n) => ({ ...n, local: !!n.local_hostname && n.local_hostname.toLowerCase() === host }));
  if (cfg.fleet?.repo) cfg.fleet.repo = expand(cfg.fleet.repo);
  cfg.file = file;
  return cfg;
}

// 初回起動: 台帳が無ければ見本を置いて、それを返す
function ensureConfig() {
  if (!fs.existsSync(USER_CONFIG)) {
    fs.mkdirSync(path.dirname(USER_CONFIG), { recursive: true });
    fs.copyFileSync(EXAMPLE, USER_CONFIG);
    return { created: true, file: USER_CONFIG };
  }
  return { created: false, file: USER_CONFIG };
}

module.exports = { loadConfig, ensureConfig, expand, USER_CONFIG };
