// 実行できる最適化の許可リスト。ここに無い操作は実行しない。
// 呼び出し側（main.js）が確認ダイアログで操作者の承認を取り、その直前に台帳と保護リストを読み直してから execute する。
//
// プロセス終了は NeonMonitor 1.1.x のレビューで見つかった失敗の型を避ける（docs/safety.md）:
//   - 古い判断のまま終了しない: 終了の直前に、同じ PID が同じ名前・同じ起動時刻のままかを確かめる（PID の再利用対策）
//   - 回復していれば終了しない: 直前に負荷を測り直し、下がっていれば中止する
//   - 保護リストを読めなければ終了しない: 呼び出し側が毎回読み直し、失敗したら plan まで進まない
//   - 終了の成功を断定しない: 5秒以内に消えたことを確認できなければ「終了未確認」として返す
'use strict';
const { execOn } = require('./collect');
const { NO_KILL } = require('./rules');

const GUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const PROC_NAME = /^[\w .()+-]{1,80}$/;
const MAC_LSTART = /^[A-Za-z]{3} [A-Za-z]{3} [ \d]\d \d{2}:\d{2}:\d{2} \d{4}$/;
const WIN_START = /^\d{17}$/;

// 終了スクリプトの結果コード
const EXIT = { 0: '終了を確認した', 3: '別のプロセスに入れ替わっていたので中止した', 4: '負荷が回復していたので中止した', 5: '終了を指示したが5秒以内に終了を確認できなかった（終了未確認）', 6: '終了の指示が失敗した' };

function isProtected(name, protect = []) {
  return NO_KILL.test(name) || protect.some((p) => String(p).toLowerCase().replace(/\.exe$/, '') === name.toLowerCase());
}

function macKillScript({ pid, name, start, min_cpu }) {
  return [
    `n=$(basename "$(ps -p ${pid} -o comm= 2>/dev/null)" 2>/dev/null)`,
    `[ "$n" = '${name}' ] || { echo "PID ${pid} は今 \${n:-存在しない}"; exit 3; }`,
    start ? `[ "$(ps -p ${pid} -o lstart= | sed 's/^ *//;s/ *$//')" = '${start}' ] || { echo "PID ${pid} は別の起動時刻のプロセス"; exit 3; }` : ':',
    `c=$(ps -p ${pid} -o pcpu= | awk '{print int($1)}')`,
    `[ "\${c:-0}" -ge ${min_cpu} ] || { echo "負荷が回復している（\${c}%）"; exit 4; }`,
    `kill -TERM ${pid} || exit 6`,
    'i=0; while [ $i -lt 10 ]; do sleep 0.5; kill -0 ' + pid + ' 2>/dev/null || { echo "終了を確認（$((i/2+1))秒以内）"; exit 0; }; i=$((i+1)); done',
    'echo "5秒以内に終了を確認できない"; exit 5',
  ].join('\n');
}

// PowerShell 本体。シングルクォートを含めない（ssh 越しに bash の '...' で包むため）
function winKillPs({ pid, name, start, min_cpu }) {
  return [
    `$p = Get-Process -Id ${pid} -ErrorAction SilentlyContinue`,
    `if (-not $p -or $p.ProcessName -ne "${name}") { Write-Output ("PID ${pid} is now " + $p.ProcessName); exit 3 }`,
    start ? `if ($p.StartTime.ToUniversalTime().ToString("yyyyMMddHHmmssfff") -ne "${start}") { Write-Output "PID ${pid} has a different start time"; exit 3 }` : '',
    '$c1 = $p.CPU; Start-Sleep -Milliseconds 1000; $p.Refresh(); $core = ($p.CPU - $c1) * 100',
    `if ($core -lt ${min_cpu}) { Write-Output ("recovered: " + [math]::Round($core) + "% of one core"); exit 4 }`,
    `try { Stop-Process -Id ${pid} -Force -ErrorAction Stop } catch { Write-Output $_.Exception.Message; exit 6 }`,
    `try { Wait-Process -Id ${pid} -Timeout 5 -ErrorAction Stop } catch { if (Get-Process -Id ${pid} -ErrorAction SilentlyContinue) { Write-Output "not confirmed within 5s"; exit 5 } }`,
    'Write-Output "terminated"; exit 0',
  ].filter(Boolean).join('; ');
}

// Windows の ssh 既定シェルは Git Bash で、/xxx 形式の引数をパスに書き換える。powercfg などは -xxx 形式で渡す
const SPECS = {
  'kill-process': {
    build(node, { pid, name, start = null, min_cpu = 50 }, { protect }) {
      if (!Number.isInteger(pid) || pid <= 4) throw new Error('PID が不正');
      if (typeof name !== 'string' || !PROC_NAME.test(name) || name.includes("'")) throw new Error('プロセス名が不正');
      if (isProtected(name, protect)) throw new Error(`${name} は保護対象（終了しない）`);
      // min_cpu = 0 は負荷の再確認をしない（プロセス一覧から操作者が明示的に選んだとき）
      if (!Number.isInteger(min_cpu) || min_cpu < 0 || min_cpu > 10000) throw new Error('負荷のしきい値が不正');
      if (start != null && !(node.os === 'macos' ? MAC_LSTART : WIN_START).test(start)) throw new Error('起動時刻が不正');
      const p = { pid, name, start, min_cpu };
      return {
        describe: `${node.id} の ${name}（PID ${pid}）を終了する。直前に同じプロセスか${min_cpu ? `・まだ重いか（1コアの ${min_cpu}% 以上）` : ''}を確かめ、違えば中止する。保存していない作業は失われ、元には戻せない。`,
        script: node.os === 'macos' ? macKillScript(p) : winKillPs(p),
        shell: node.os === 'macos' ? 'sh' : 'ps',
        exits: EXIT,
      };
    },
  },
  'set-power-plan': {
    windowsOnly: true,
    build(node, { guid, prev_guid }) {
      if (!GUID.test(guid) || (prev_guid && !GUID.test(prev_guid))) throw new Error('GUID が不正');
      return {
        describe: `${node.id} の電源プランを切り替える（powercfg -setactive ${guid}）。元に戻す操作を記録する。`,
        script: `powercfg.exe -setactive ${guid}; powercfg.exe -getactivescheme`,
        shell: 'ps',
        undo: prev_guid ? { type: 'set-power-plan', params: { guid: prev_guid } } : null,
      };
    },
  },
};

// 定期処理の管理。Windows はタスクスケジューラ、macOS は自分のユーザーの LaunchAgents だけ（system の daemon は触らない）
const TASK_PATH = /^\\([\w .-]+\\)*$/;
const TASK_NAME = /^[\w .()+-]{1,120}$/;
const LABEL = /^[\w.-]{1,150}$/;
const LAUNCH_AGENTS = /^\/Users\/[\w.-]+\/Library\/LaunchAgents\/[\w.-]+\.plist$/;

function checkTask({ path, name }) {
  if (typeof path !== 'string' || !TASK_PATH.test(path)) throw new Error('タスクのパスが不正');
  if (typeof name !== 'string' || !TASK_NAME.test(name)) throw new Error('タスク名が不正');
  if (/^\\Microsoft\\/i.test(path)) throw new Error('Windows 標準のタスクは扱わない');
}
function checkLabel({ label, plist }) {
  if (typeof label !== 'string' || !LABEL.test(label) || label.startsWith('com.apple.')) throw new Error('ラベルが不正');
  if (plist != null && (typeof plist !== 'string' || !LAUNCH_AGENTS.test(plist))) throw new Error('plist はユーザーの LaunchAgents のものだけ');
}

Object.assign(SPECS, {
  'task-disable': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を無効にする（予定どおりに動かなくなる）。元に戻せる。`, script: `Disable-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}" | Out-Null; (Get-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}").State`, shell: 'ps', undo: { type: 'task-enable', params: { path: p.path, name: p.name } } };
    },
  },
  'task-enable': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を有効にする。元に戻せる。`, script: `Enable-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}" | Out-Null; (Get-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}").State`, shell: 'ps', undo: { type: 'task-disable', params: { path: p.path, name: p.name } } };
    },
  },
  'task-run': {
    windowsOnly: true,
    build(node, p) {
      checkTask(p);
      return { describe: `${node.id} のタスク「${p.path}${p.name}」を今すぐ1回実行する。中身（バックアップ・同期など）が実際に動く。元には戻せない。`, script: `Start-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}"; Start-Sleep -Seconds 2; (Get-ScheduledTask -TaskPath "${p.path}" -TaskName "${p.name}").State`, shell: 'ps' };
    },
  },
  'launchd-unload': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      if (!p.plist) throw new Error('元に戻すための plist のパスが要る');
      return { describe: `${node.id} の launchd ジョブ ${p.label} を止めて読み込みを外す（次のログインまで、または元に戻すまで動かない）。`, script: `launchctl bootout gui/$(id -u)/${p.label} && echo unloaded`, shell: 'sh', undo: { type: 'launchd-load', params: { label: p.label, plist: p.plist } } };
    },
  },
  'launchd-load': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      if (!p.plist) throw new Error('plist のパスが要る');
      return { describe: `${node.id} の launchd ジョブ ${p.label} を読み込む。`, script: `launchctl bootstrap gui/$(id -u) '${p.plist}' && echo loaded`, shell: 'sh', undo: { type: 'launchd-unload', params: { label: p.label, plist: p.plist } } };
    },
  },
  'launchd-kickstart': {
    macOnly: true,
    build(node, p) {
      checkLabel(p);
      return { describe: `${node.id} の launchd ジョブ ${p.label} を今すぐ1回実行する（動いていれば再起動しない）。中身が実際に動く。`, script: `launchctl kickstart gui/$(id -u)/${p.label} && echo started`, shell: 'sh' };
    },
  },
});

// ctx.protect: 実行の直前に読み直した保護リスト。読めなかったら呼び出し側で止める（ここには来ない）
function plan(node, action, ctx = {}) {
  const spec = SPECS[action?.type];
  if (!spec) throw new Error(`許可リストに無い操作: ${action?.type}`);
  if (node.shared) throw new Error(`${node.id} は共用機のため、このアプリからは変更しない`);
  if (spec.windowsOnly && node.os !== 'windows') throw new Error('Windows だけの操作');
  if (spec.macOnly && node.os !== 'macos') throw new Error('macOS だけの操作');
  if (!Array.isArray(ctx.protect)) throw new Error('保護リストを読めないので実行しない');
  return spec.build(node, action.params || {}, ctx);
}

// 実際に送るコマンド。Windows のリモートは Git Bash 経由なので PowerShell を '...' で包む
function wrap(node, p) {
  if (p.shell === 'sh' || node.local) return p.script;
  if (p.script.includes("'")) throw new Error('PowerShell 本体にシングルクォートは使えない');
  return `powershell.exe -NoProfile -NonInteractive -Command '${p.script}'`;
}

// exec: 機体でコマンドを実行する口（既定は ssh / ローカルシェル。テストが偽物に差し替える）
async function execute(node, action, ctx, exec = execOn) {
  const p = plan(node, action, ctx);
  const res = await exec(node, wrap(node, p), 30000);
  return {
    ok: res.code === 0, code: res.code,
    outcome: p.exits?.[res.code] || (res.code === 0 ? '完了' : `失敗（exit ${res.code}）`),
    output: (res.out + res.err).trim().slice(-1500),
    undo: res.code === 0 ? p.undo || null : null,
  };
}

module.exports = { plan, execute, wrap, SPECS, EXIT };
