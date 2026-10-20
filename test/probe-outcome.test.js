'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
const { openDb } = require('../lib/db');
const ledger = require('../lib/ledger');

test('Electron ledger keeps the latest failure after reopening the DB and clears it after recovery', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-probe-outcome-'));
  let db = openDb(dir);
  try {
    const snapshot = (at) => ({ at, data: { probe: 'windows' }, summary: { bench_ms: null, cpu: null, mem_avail: null, swap_gb: null, critical: 0, warn: 0 } });
    db.addSnapshot('dev', snapshot(10));
    const source = fs.readFileSync(require.resolve('../main'), 'utf8');
    await vm.runInNewContext(source.slice(source.indexOf('async function runProbe('), source.indexOf('async function syncLogs(')) + '\nrunProbe();', {
      probing: false, updateTray() {}, reloadConfig() {}, computeChecks() {},
      cfg: { nodes: [{ id: 'dev', os: 'windows' }] }, db, ledger, lastProbeError: {}, win: null,
      enrich: (r) => r,
      probeAll: async (_nodes, result) => { result({ node_id: 'dev', ok: false, at: 20, error: 'ssh: timeout' }); return [{}]; },
    });
    db.saveProbeFailure('dev', 15, 'older failure');
    db.db.close();
    db = openDb(dir);
    let handler;
    vm.runInNewContext(source.slice(source.indexOf("ipcMain.handle('ledger'"), source.indexOf("ipcMain.handle('last'")), {
      ipcMain: { handle: (_name, fn) => { handler = fn; } }, reloadConfig() {},
      cfg: { nodes: [{ id: 'dev', os: 'windows' }] }, db, ledger, lastProbeError: {},
    });
    assert.equal(handler().rows[0].status, 'unobserved');
    assert.match(handler().rows[0].reason, /ssh: timeout/);
    db.addSnapshot('dev', snapshot(30));
    assert.equal(handler().rows[0].status, 'target');
    db.saveProbeFailure('dev', 30, 'same timestamp failure');
    assert.equal(handler().rows[0].status, 'unobserved');
  } finally {
    db.db.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('Electron last restores latest failed outcomes, including failure before the first success', () => {
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'kt-last-outcome-'));
  let db=openDb(dir);
  try {
    const snapshot=at=>({at,data:{probe:'windows'},summary:{bench_ms:null,cpu:null,mem_avail:null,swap_gb:null,critical:0,warn:0}});
    db.addSnapshot('dev',snapshot(10));db.saveProbeFailure('dev',20,'probe failed');db.saveProbeFailure('new',20,'initial probe failed');
    db.db.close();db=openDb(dir);
    const source=fs.readFileSync(require.resolve('../main'),'utf8');let handler;
    vm.runInNewContext(source.slice(source.indexOf("ipcMain.handle('last'"),source.indexOf("ipcMain.handle('probe'")),{
      ipcMain:{handle:(_name,fn)=>{handler=fn;}},cfg:{nodes:[{id:'dev'},{id:'new'}]},db,enrich:r=>r,
    });
    const rows=handler();assert.equal(rows.length,2);assert.equal(rows[0].ok,false);assert.equal(rows[0].at,20);assert.equal(rows[1].ok,false);
    db.addSnapshot('dev',snapshot(30));assert.equal(handler()[0].ok,true);assert.equal(handler()[0].at,30);
    db.saveProbeFailure('dev',30,'same timestamp failure');assert.equal(handler()[0].ok,false);
  } finally {db.db.close();fs.rmSync(dir,{recursive:true,force:true});}
});
