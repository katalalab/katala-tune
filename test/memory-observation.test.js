'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { spawnSync } = require('node:child_process');

function testEnvironment() {
  const env = {};
  for (const key of ['PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'HOME', 'USERPROFILE']) {
    if (process.env[key] != null) env[key] = process.env[key];
  }
  env.PYTHONDONTWRITEBYTECODE = '1';
  return env;
}

test('Mac probe は未取得の swap と実際の 0 を区別する', () => {
  const python = process.env.PYTHON || (process.platform === 'win32' ? 'python' : 'python3');
  const script = String.raw`
import ast, json, math, re
from pathlib import Path
tree = ast.parse(Path('probes/mac_probe.py').read_text(encoding='utf-8'))
functions = [x for x in tree.body if isinstance(x, ast.FunctionDef) and x.name in ('num', 'memory')]
assert len(functions) == 2
values = {'hw.pagesize':'16384', 'hw.memsize':'17179869184', 'kern.memorystatus_vm_pressure_level':'1', 'kern.memorystatus_level':'50'}
ns = {'re':re, 'math':math, 'run':lambda *args, **kwargs:'', 'sysctl':lambda key:values.get(key,'')}
exec(compile(ast.Module(body=functions, type_ignores=[]), '<fixture>', 'exec'), ns)
rows=[]
for sample in ['', 'total = 0.00M  used = 0.00M', 'total = 2048.00M  used = 1024.00M', 'total = 2048.00M', 'total = 1..2M  used = 0.00M']:
    values['vm.swapusage']=sample
    result=ns['memory']()
    rows.append([result['swap_used_gb'],result['swap_total_gb']])
print(json.dumps(rows))
`;
  const result = spawnSync(python, ['-I', '-B', '-S', '-c', script], {
    cwd: path.join(__dirname, '..'), encoding: 'utf8', windowsHide: true, timeout: 10_000,
    env: testEnvironment(),
  });
  assert.equal(result.status, 0, 'synthetic Python probe failed');
  const samples = JSON.parse(result.stdout);
  assert.deepEqual(samples, [[null, null], [0, 0], [1, 2], [null, 2], [0, null]]);
  const { nodeChecks } = require('../lib/health');
  const now = 1_800_000_000_000;
  const status = ([used, total]) => nodeChecks({ id: 'fixture' }, {
    at: now, data: { probe: 'mac', memory: { total_gb: 16, available_pct: 50, pressure: 'normal', swap_used_gb: used, swap_total_gb: total } },
  }, [], { now }).find((check) => check.id === 'memory').status;
  assert.equal(status(samples[0]), 'unknown');
  assert.equal(status(samples[1]), 'ok');
});

test('Windows probe は未取得の値と実際の 0 を区別する', { skip: process.platform !== 'win32' }, () => {
  const powershell = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  const result = spawnSync(powershell, ['-NoProfile', '-NonInteractive', '-File', path.join(__dirname, 'win-memory-observation.ps1')], {
    cwd: path.join(__dirname, '..'), encoding: 'utf8', windowsHide: true, timeout: 10_000, env: testEnvironment(),
  });
  assert.equal(result.status, 0, 'synthetic PowerShell probe failed');
  assert.match(result.stdout, /PASS: synthetic Windows memory observations/);
});

function renderResources(probe, memory) {
  const text = fs.readFileSync(path.join(__dirname, '..', 'renderer', 'app.js'), 'utf8');
  const start = text.indexOf('function renderResources() {');
  const end = text.indexOf('\n// ---- プロセス', start);
  assert.ok(start >= 0 && end > start);
  let rendered = '';
  const data = { probe, memory, host: { cpu: 'fixture', uptime_h: null }, gpus: [], cpu_busy: null };
  const context = {
    state: { nodes: [{ id: 'fixture' }], results: { fixture: { ok: true, data, score: 0 } } },
    ticket() {}, setCrumbs() {}, bindGoto() {}, page(value) { rendered = value; },
    Live: { toggle: () => '', panel: () => '', mount() {} },
    UI: { head: () => '', table: ({ rows, row }) => rows.map(row).join(''), empty: () => '' },
    Charts: { meter: () => '', ring: () => '' }, TH: { cpu: {}, mem: {}, disk: {} },
    esc: (value) => String(value ?? ''), sysDisk: () => null, withUnit: () => '-',
  };
  vm.runInNewContext(`${text.slice(start, end)}\nrenderResources();`, context);
  return rendered;
}

test('リソース画面は未取得値を不明とし、実測の 0 は残す', () => {
  assert.match(renderResources('mac', { available_pct: 50, swap_used_gb: null }), /<td class="n mid">不明<\/td>/);
  assert.match(renderResources('windows', { available_pct: 50, commit_pct: null }), /<td class="n mid">不明<\/td>/);
  assert.match(renderResources('windows', null), /<td class="n mid">不明<\/td>/);
  assert.match(renderResources('mac', { available_pct: 50, swap_used_gb: 0 }), /<td class="n mid">0 GB<\/td>/);
  assert.match(renderResources('windows', { available_pct: 50, commit_pct: 0 }), />0%<\/span>/);
});

function renderMachine(probe, memory) {
  const text = fs.readFileSync(path.join(__dirname, '..', 'renderer', 'app.js'), 'utf8');
  const start = text.indexOf('function machineHtml(d, r) {');
  const end = text.indexOf('\nconst withUnit', start);
  assert.ok(start >= 0 && end > start);
  const data = { probe, memory, host: { os: 'fixture', cpu: 'fixture', cores: 1, uptime_h: null }, disk: [], gpus: [] };
  const context = { data, UI: { props: (rows) => rows.find(([key]) => key === 'メモリ')[1] }, esc: (value) => String(value ?? ''), gpuHtml: () => '', Charts: {}, TH: {} };
  return vm.runInNewContext(`${text.slice(start, end)}\nmachineHtml(data, {});`, context);
}

test('機体詳細もメモリ欠測を不明と示す', () => {
  assert.match(renderMachine('mac', { total_gb: 16, available_pct: 50, swap_used_gb: null }), /swap 不明 GB/);
  assert.match(renderMachine('windows', { total_gb: null, available_pct: null, commit_pct: null }), /不明 GB · 空き 不明% · コミット 不明%/);
  assert.match(renderMachine('windows', null), /不明 GB · 空き 不明% · コミット 不明%/);
  assert.match(renderMachine('mac', { total_gb: 16, available_pct: 50, swap_used_gb: 0 }), /swap 0 GB/);
});

function renderOverviewParts(memory, findings = [], probe = 'windows') {
  const text = fs.readFileSync(path.join(__dirname, '..', 'renderer', 'app.js'), 'utf8');
  const cardStart = text.indexOf('function cardHtml(n, coverage) {');
  const cardEnd = text.indexOf('\nfunction renderOverview()', cardStart);
  const propsStart = text.indexOf('function nodeProps(n, r) {');
  const propsEnd = text.indexOf('\nasync function renderNode(', propsStart);
  assert.ok(cardStart >= 0 && cardEnd > cardStart && propsStart >= 0 && propsEnd > propsStart);
  const helperStart = text.indexOf('function memoryObservationKnown(');
  const helperEnd = helperStart < 0 ? -1 : text.indexOf('\nfunction nodeDot(', helperStart);
  const helper = helperStart >= 0 && helperEnd > helperStart ? text.slice(helperStart, helperEnd) : '';
  const node = { id: 'fixture', os: probe };
  const result = { ok: true, score: 100, findings, data: { probe, memory, host: { cpu: 'fixture', cores: 1 }, disk: [] } };
  const context = {
    node, result,
    state: { results: { fixture: result }, busy: new Set() },
    fleetNode: () => ({ metrics: { mem: 77, cpu: 66, disk: 55 } }),
    counts: (r) => ({ critical: r.findings.filter((f) => f.severity === 'critical').length, warn: r.findings.filter((f) => f.severity === 'warn').length, info: 0 }),
    sysDisk: () => null, memoryObservationKnown: () => false,
    UI: { chip: (label, tone) => `[${label}|${tone}]`, props: (rows) => rows.filter(Boolean).map((row) => row.join(':')).join('|') },
    Charts: { ring: () => '[score]', meter: (value, options) => `<meter label="${options.label}" value="${value ?? 'unknown'}">` },
    TH: { cpu: {}, mem: {}, disk: {} }, esc: (value) => String(value ?? ''), icon: () => '', osIcon: () => '', opChip: () => '',
    scoreClass: () => '', ago: () => '', fmtTime: () => '', compareHtml: () => '', COVERAGE_LABEL: { fresh: '新鮮' }, COVERAGE_TONE: { fresh: 'green' },
  };
  return vm.runInNewContext(`${helper}\n${text.slice(cardStart, cardEnd)}\n${text.slice(propsStart, propsEnd)}\n[cardHtml(node, 'fresh'), nodeProps(node, result)]`, context);
}

test('メモリ欠測のカードと詳細は良好と表示せず別データを混ぜない', () => {
  const [card, details] = renderOverviewParts({ total_gb: null, available_pct: null, commit_pct: null });
  assert.match(card, /メモリ不明/);
  assert.match(details, /メモリ不明/);
  assert.doesNotMatch(card + details, /良好/);
  assert.match(card, /label="メモリ" value="unknown"/);
  assert.match(card, /label="CPU" value="unknown"/);
  assert.match(card, /label="ディスク" value="unknown"/);
  assert.match(card + details, /不明 GB/);
});

test('注意がある場合もメモリの欠測を併記する', () => {
  const [card, details] = renderOverviewParts({ total_gb: 64, available_pct: 50, commit_pct: null }, [{ severity: 'warn' }]);
  assert.match(card, /注意 1/);
  assert.match(details, /注意 1/);
  assert.match(card, /メモリ不明/);
  assert.match(details, /メモリ不明/);
});

test('OS 不明な記録の危険値は概要でも消えない', () => {
  const memory = { commit_pct: 95 };
  const findings = require('../lib/rules').analyze({ memory });
  const [card, details] = renderOverviewParts(memory, findings, null);
  assert.match(card, /重大 1/);
  assert.match(details, /重大 1/);
  assert.match(card + details, /メモリ不明/);
});

test('Mac のカードもメモリの取得漏れを示す', () => {
  const [missing] = renderOverviewParts({ total_gb: 16, available_pct: null, pressure: 'normal', swap_used_gb: 0 }, [], 'mac');
  const [known] = renderOverviewParts({ total_gb: 16, available_pct: 50, pressure: 'normal', swap_used_gb: 0 }, [], 'mac');
  assert.match(missing, /メモリ不明/);
  assert.doesNotMatch(known, /メモリ不明/);
});

test('所見がない場合も全体の正常性やスコアの取得範囲を断定しない', () => {
  const [card, details] = renderOverviewParts({ total_gb: 64, available_pct: 50, commit_pct: 30 });
  assert.match(card, /所見なし/);
  assert.match(details, /所見なし/);
  assert.doesNotMatch(card + details, /良好/);
  assert.match(card, /取得範囲は未確認/);
  assert.match(details, /取得範囲は未確認/);
});

function renderDashboardLast(last) {
  const text = fs.readFileSync(path.join(__dirname, '..', 'renderer', 'dashboard.js'), 'utf8');
  const start = text.indexOf('  function mdmTable(d) {');
  const end = text.indexOf('\n  function alertsList(', start);
  assert.ok(start >= 0 && end > start);
  const data = { nodes: [{ id: 'fixture', os: 'windows', last }] };
  const context = {
    data, esc: (value) => String(value ?? ''), num: Number.isFinite, r0: Math.round, fmtUptime: () => '', ago: () => '',
    UI: { chip: (label, tone) => `[${label}|${tone}]`, table: ({ cols, rows, row }) => cols.map((col) => col.label).join('|') + rows.map(row).join('') },
  };
  return vm.runInNewContext(`${text.slice(start, end)}\nmdmTable(data);`, context);
}

test('ダッシュボードの所見欄も分析の完全性を断定しない', () => {
  const html = renderDashboardLast({ at: 1, ok: true, findings_known: true, score: 100, critical: 0, warn: 0 });
  assert.doesNotMatch(html, /良好/);
  assert.match(html, /所見なし/);
  assert.match(html, /取得範囲未確認/);
});

test('ダッシュボードは分析失敗や所見不明を所見なしと区別する', () => {
  const failed = renderDashboardLast({ at: 1, ok: false, critical: 0, warn: 0 });
  const unknown = renderDashboardLast({ at: 1, critical: 0, warn: 0 });
  const incomplete = renderDashboardLast({ at: 1, ok: true, findings_known: false, critical: 0, warn: 0 });
  assert.match(failed, /分析失敗/);
  assert.match(unknown, /所見不明/);
  assert.match(incomplete, /所見不明/);
  assert.doesNotMatch(failed + unknown + incomplete, /所見なし|良好/);
});
