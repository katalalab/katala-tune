'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { plan, wrap } = require('../lib/actions');

const mac = { id: 'm', alias: 'm', os: 'macos' };
const win = { id: 'w', alias: 'w', os: 'windows' };
const ctx = { protect: ['python.exe', 'Code'] };

test('許可リストに無い操作は拒否する', () => {
  assert.throws(() => plan(mac, { type: 'rm-rf', params: {} }, ctx), /許可リスト/);
});

test('共用機では何も実行しない', () => {
  assert.throws(() => plan({ ...win, shared: true }, { type: 'kill-process', params: { pid: 100, name: 'foo' } }, ctx), /共用機/);
});

test('保護リストを読めないとき（ctx.protect が無い）は実行しない', () => {
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: 500, name: 'Google Drive' } }, {}), /保護リスト/);
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: 500, name: 'Google Drive' } }), /保護リスト/);
});

test('保護リストにあるアプリは終了しない（.exe の有無・大文字小文字を問わない）', () => {
  assert.throws(() => plan(win, { type: 'kill-process', params: { pid: 700, name: 'python' } }, ctx), /保護対象/);
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: 700, name: 'code' } }, ctx), /保護対象/);
});

test('プロセス終了: 名前・起動時刻・負荷を直前に確かめ、終了を待って結果を断定しない', () => {
  const p = plan(mac, { type: 'kill-process', params: { pid: 500, name: 'Google Drive', start: 'Thu Oct  1 08:12:03 2026', min_cpu: 50 } }, ctx);
  assert.match(p.script, /ps -p 500 -o comm=/);
  assert.match(p.script, /ps -p 500 -o lstart=.*'Thu Oct  1 08:12:03 2026'.*exit 3/);
  assert.match(p.script, /-ge 50 \].*exit 4/);
  assert.match(p.script, /kill -TERM 500/);
  assert.match(p.script, /kill -0 500/);
  assert.match(p.script, /exit 5$/);
  const w = plan(win, { type: 'kill-process', params: { pid: 700, name: 'PresentMon_x64', start: '20261003004120123', min_cpu: 50 } }, ctx);
  assert.match(w.script, /ProcessName -ne "PresentMon_x64".*exit 3/);
  assert.match(w.script, /yyyyMMddHHmmssfff"\) -ne "20261003004120123".*exit 3/);
  assert.match(w.script, /\$core -lt 50.*exit 4/);
  assert.match(w.script, /Stop-Process -Id 700/);
  assert.match(w.script, /Wait-Process -Id 700 -Timeout 5.*exit 5/);
  assert.ok(!w.script.includes("'"), 'PowerShell 本体にシングルクォートが無い');
  assert.match(wrap(win, w), /^powershell\.exe -NoProfile -NonInteractive -Command '/);
  assert.equal(wrap({ ...win, local: true }, w), w.script);
});

test('プロセス終了: 引数の注入を拒否する', () => {
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: '500; rm -rf ~', name: 'x' } }, ctx), /PID/);
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: 500, name: "x'; rm -rf ~; '" } }, ctx), /プロセス名/);
  assert.throws(() => plan(mac, { type: 'kill-process', params: { pid: 500, name: 'x', start: "now'; reboot" } }, ctx), /起動時刻/);
  assert.throws(() => plan(win, { type: 'kill-process', params: { pid: 500, name: 'x', start: '2026' } }, ctx), /起動時刻/);
  assert.throws(() => plan(win, { type: 'kill-process', params: { pid: 600, name: 'MsMpEng' } }, ctx), /保護対象/);
  assert.throws(() => plan(win, { type: 'kill-process', params: { pid: 4, name: 'System' } }, ctx), /PID/);
  assert.throws(() => plan(win, { type: 'kill-process', params: { pid: 9, name: 'x', min_cpu: '1; x' } }, ctx), /しきい値/);
});

test('電源プラン: GUID と旧値を検証し、実機で再読・検証・必要時だけ復元する', () => {
  const p = plan(win, { type: 'set-power-plan', params: { guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', prev_guid: '381b4222-f694-41f0-9685-ff5bb260df2e' } }, ctx);
  assert.match(p.script, /powercfg\.exe -setactive 8c5e7fda/);
  assert.match(p.script, /\$before -ne "381b4222-f694-41f0-9685-ff5bb260df2e".*exit 3/);
  assert.match(p.script, /\$after -eq "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c".*exit 0/);
  assert.match(p.script, /\$after -eq "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c".*-setactive 381b4222/);
  assert.match(p.script, /Global\\KatalaTunePowerControl.*WaitOne\(0\).*Read-Plan-Retry/);
  assert.ok(!/ \/setactive/.test(p.script));
  assert.deepEqual(p.undo, { type: 'set-power-plan', params: { guid: '381b4222-f694-41f0-9685-ff5bb260df2e', prev_guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c' } });
  assert.throws(() => plan(win, { type: 'set-power-plan', params: { guid: 'x && shutdown' } }, ctx), /GUID/);
  assert.throws(() => plan(win, { type: 'set-power-plan', params: { guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c' } }, ctx), /GUID/);
  assert.throws(() => plan(mac, { type: 'set-power-plan', params: { guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c' } }, ctx), /Windows/);
});

test('GPU 電力制限: UUID・有限の上下限・旧値を検証し、実機で再読・検証・必要時だけ復元する', () => {
  const params = { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 };
  const p = plan(win, { type: 'set-gpu-power-limit', params }, ctx);
  assert.match(p.script, /--id=GPU-01234567-89ab-cdef-0123-456789abcdef --query-gpu=power\.min_limit,power\.max_limit,power\.limit/);
  assert.match(p.script, /\$v\[0\] -ne 100.*\$v\[1\] -ne 250.*\$v\[2\] -ne 200.*exit 3/);
  assert.match(p.script, /-pl 180/);
  assert.match(p.script, /\$after\[2\] -eq 180.*exit 0/);
  assert.match(p.script, /\$after\[2\] -eq 180.*\$after\[0\] -le 200.*\$after\[1\] -ge 200.*-pl 200/);
  assert.match(p.script, /Global\\KatalaTunePowerControl.*WaitOne\(0\).*Read-Power-Retry/);
  assert.ok(!p.script.includes("'"), 'PowerShell 本体にシングルクォートが無い');
  assert.deepEqual(p.undo, { type: 'set-gpu-power-limit', params: { ...params, watts: 200, prev_w: 180 } });
  for (const bad of [
    { ...params, uuid: '0 && shutdown' },
    { ...params, watts: Infinity },
    { ...params, watts: 99 },
    { ...params, min: 251 },
    { ...params, prev_w: 251 },
  ]) assert.throws(() => plan(win, { type: 'set-gpu-power-limit', params: bad }, ctx), /GPU|電力/);
  assert.throws(() => plan(mac, { type: 'set-gpu-power-limit', params }, ctx), /Windows/);
});

test('macOS 低電力モード: 電源ドメインの旧値を再読し、検証失敗時は変更済みだけ復元する', () => {
  const params = { source: 'battery', enabled: true, prev: false };
  const p = plan(mac, { type: 'set-low-power-mode', params }, ctx);
  assert.match(p.script, /pmset -g custom/);
  assert.match(p.script, /power-action\.lock/);
  assert.match(p.script, /lowpowermode/);
  assert.match(p.script, /Battery Power/);
  assert.match(p.script, /\[ "\$before_mode" = 0 \] \|\| \{.*exit 3/);
  assert.match(p.script, /pmset -b "\$before_key" 1/);
  assert.match(p.script, /\[ "\$after_key" = "\$before_key" \].*\[ "\$after_mode" = 1 \].*exit 0/);
  assert.ok(!/sudo|osascript/.test(p.script));
  assert.deepEqual(p.undo, { type: 'set-low-power-mode', params: { source: 'battery', enabled: false, prev: true } });
  assert.throws(() => plan(mac, { type: 'set-low-power-mode', params: { ...params, source: 'usb' } }, ctx), /電源/);
  assert.throws(() => plan(mac, { type: 'set-low-power-mode', params: { ...params, prev: 'false' } }, ctx), /真偽/);
});

test('タスク管理: Windows 標準のタスクと不正な名前は拒否し、無効化は有効化で戻せる', () => {
  const p = plan(win, { type: 'task-disable', params: { path: '\\', name: 'KatalaGitHubBackup' } }, ctx);
  assert.match(p.script, /Disable-ScheduledTask -TaskPath "\\" -TaskName "KatalaGitHubBackup"/);
  assert.deepEqual(p.undo, { type: 'task-enable', params: { path: '\\', name: 'KatalaGitHubBackup' } });
  assert.throws(() => plan(win, { type: 'task-disable', params: { path: '\\Microsoft\\Windows\\', name: 'Defrag' } }, ctx), /標準/);
  assert.throws(() => plan(win, { type: 'task-run', params: { path: '\\', name: 'x"; Remove-Item C:\\ -Recurse; "' } }, ctx), /タスク名/);
  assert.throws(() => plan(win, { type: 'task-run', params: { path: 'C:\\', name: 'x' } }, ctx), /パス/);
  assert.throws(() => plan(mac, { type: 'task-run', params: { path: '\\', name: 'x' } }, ctx), /Windows/);
  assert.throws(() => plan({ ...win, shared: true }, { type: 'task-run', params: { path: '\\', name: 'x' } }, ctx), /共用機/);
});

test('launchd: ユーザーの LaunchAgents だけを扱い、止める操作は読み込みで戻せる', () => {
  const plist = '/Users/me/Library/LaunchAgents/com.example.job.plist';
  const p = plan(mac, { type: 'launchd-unload', params: { label: 'com.example.job', plist } }, ctx);
  assert.match(p.script, /launchctl bootout gui\/\$\(id -u\)\/com\.example\.job/);
  assert.deepEqual(p.undo, { type: 'launchd-load', params: { label: 'com.example.job', plist } });
  assert.throws(() => plan(mac, { type: 'launchd-unload', params: { label: 'com.apple.Finder', plist } }, ctx), /ラベル/);
  assert.throws(() => plan(mac, { type: 'launchd-load', params: { label: 'x', plist: '/Library/LaunchDaemons/x.plist' } }, ctx), /LaunchAgents/);
  assert.throws(() => plan(mac, { type: 'launchd-kickstart', params: { label: 'x; rm -rf ~' } }, ctx), /ラベル/);
  assert.throws(() => plan(win, { type: 'launchd-kickstart', params: { label: 'x' } }, ctx), /macOS/);
});
