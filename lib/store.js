// 調査結果と実行記録をローカルに残す（JSON Lines）。前回比と「元に戻す」に使う
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const KEEP = 60;

function createStore(dir) {
  fs.mkdirSync(path.join(dir, 'history'), { recursive: true });
  const histFile = (id) => path.join(dir, 'history', `${id.replace(/[^\w.-]/g, '_')}.jsonl`);
  const actionsFile = path.join(dir, 'actions.jsonl');

  const readLines = (f) => {
    if (!fs.existsSync(f)) return [];
    return fs.readFileSync(f, 'utf8').split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
  };

  return {
    dir,
    history(id) { return readLines(histFile(id)); },
    addSnapshot(id, entry) {
      const rows = [...readLines(histFile(id)), entry].slice(-KEEP);
      fs.writeFileSync(histFile(id), rows.map((r) => JSON.stringify(r)).join('\n') + '\n');
    },
    actions() { return readLines(actionsFile); },
    addAction(entry) { fs.appendFileSync(actionsFile, JSON.stringify(entry) + '\n'); },
  };
}

module.exports = { createStore };
