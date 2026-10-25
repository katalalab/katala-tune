// snapshot（probe の出力）から所見と提案を作る。副作用なし。
// finding = { id, severity: critical|warn|info, category, title, detail, advice, commands?, action? }
'use strict';
const netsec = require('./netsec');
const wslInside = require('./wsl');

const HIGH_PERF_GUID = '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c';
const BALANCED_GUID = '381b4222-f694-41f0-9685-ff5bb260df2e';
const POWER_SAVER_GUID = 'a1841308-3541-4fab-bc81-f71556f20b4a';
// 登録し直すコマンドを添えるのは、ユーザーの LaunchAgents の plist のときだけ（lib/actions.js と同じ形）
const LAUNCH_AGENTS = /^\/Users\/[\w.-]+\/Library\/LaunchAgents\/[\w.-]+\.plist$/;

// 終了の提案を出さないプロセス。OS の中核、セキュリティ、作業中の AI エージェントや実行環境
const NO_KILL = new RegExp('^(' + [
  'kernel_task', 'launchd', 'WindowServer', 'loginwindow', 'Finder', 'Dock', 'SystemUIServer', 'mds', 'mds_stores', 'mdworker.*',
  'fileproviderd', 'cloudd', 'bird', 'coreaudiod', 'JamfDaemon', 'Jamf.*', 'com\\.apple\\..*', 'Virtualization.*',
  'System', 'Idle', 'Registry', 'smss', 'csrss', 'wininit', 'services', 'lsass', 'svchost', 'winlogon', 'dwm', 'explorer',
  'MsMpEng', 'NisSrv', 'SecurityHealth.*', 'Memory Compression', 'vmmem.*', 'vmcompute', 'WmiPrvSE', 'fontdrvhost', 'sihost',
  'ctfmon', 'audiodg', 'spoolsv', 'conhost', 'SearchIndexer', 'TiWorker', 'TrustedInstaller', 'MsSense', 'sshd', 'ssh.*',
  'bash', 'zsh', 'powershell', 'pwsh', 'python3?(\\.\\d+)?', 'node', 'claude', 'codex', 'opencode', 'cursor-agent', 'agy',
  'op', 'op-agent', '1Password.*', 'tailscale.*', 'Tailscale.*', 'docker.*', 'com\\.docker\\..*', 'colima', 'limactl', 'qemu.*',
  'ollama.*', 'llama-server', 'nvcontainer', 'NVDisplay.*',
].join('|') + ')$', 'i');

// 名前ごとの対処の知見（app 名かプロセス名に当てる）
const ADVICE = [
  [/^Google Drive$|^fileproviderd$/, 'Google Drive for desktop の同期が CPU を使い続けている。Drive を終了して再起動し、それでも続くならミラーリング対象を減らすかストリーミングに切り替える。'],
  [/^biomesyncd$|^BiomeAgent$/, 'macOS の利用状況（Screen Time 等）の同期。iCloud 同期の再試行で張り付くことがある。数時間続くなら再ログインか再起動で収まる。'],
  [/^(Google Chrome|chrome)$/, 'タブとプロファイルの数に比例する。chrome://settings/performance の「メモリセーバー」を有効にし、使っていないプロファイルのウィンドウを閉じる。'],
  [/^vmmemWSL$|^vmmem$/, 'WSL の VM。GPU 計算 PID がある・GPU 使用率が高い・GPU 状態を取得できない、または GPU 学習保護の機体では、`wsl --shutdown` と上限変更の反映を保留する。使い終わったことを確認してからメモリを返す。常時大きいなら %UserProfile%\\.wslconfig の memory で上限を付ける。'],
  [/^(Discord|Slack|Microsoft Teams|ChatGPT|Claude|Notion|Spotify)$/, 'Electron 系アプリ。開きっぱなしのワークスペースやウィンドウが多いほど重い。使わない時間帯は終了する。'],
  [/^WindowServer$/, '画面描画。外部ディスプレイの高解像度スケーリングや、透明効果・大量のウィンドウで上がる。「視差効果を減らす」「透明度を下げる」で軽くなる。'],
  [/^(mds|mds_stores|mdworker.*|SearchIndexer)$/, '検索インデックス作成。node_modules やリポジトリ群を検索対象から外すと収まる。'],
  [/^MsMpEng$/, 'Defender のリアルタイム保護がビルドや git 操作のファイルを全部検査している。開発ディレクトリを除外リストに加える（管理者権限が要る）。'],
  [/^PresentMon/, 'NVIDIA FrameView SDK（nvfvsdksvc_x64.exe）が起動する計測プロセス。NVIDIA アプリの性能オーバーレイ・統計を使っていなければ、NVIDIA アプリ > 設定 > 機能でパフォーマンス監視を切ると止まる。'],
  [/^rust-analyzer$/, 'エディタの Rust 解析。開いている大きなワークスペースごとに1つ動く。使っていない VS Code ウィンドウを閉じる。'],
];

const adviceFor = (...names) => {
  for (const n of names) for (const [re, a] of ADVICE) if (n && re.test(n)) return a;
  return null;
};

const gb = (mb) => (mb / 1024).toFixed(1);
const fmtMb = (mb) => (mb >= 1024 ? `${gb(mb)} GB` : `${mb} MB`);
// 同一コマンドの多重起動の閾値（本数と合計 MB の両方を超えたとき）。種類は key（実行ファイル[:スクリプト名]）から決める
const MULTI = { fsmonitor: { min: 10, mb: 100 }, mcp: { min: 4, mb: 256 }, other: { min: 30, mb: 2048 } };
const multiKind = (key) => (key === 'git:fsmonitor--daemon' ? 'fsmonitor' : /mcp/i.test(key) ? 'mcp' : 'other');
const MULTI_ADVICE = {
  fsmonitor: 'git の fsmonitor デーモンがリポジトリごとに常駐している。`git config --global core.fsmonitor false` で新しく増えなくなる（提案であり、自動では変更しない）。すでに常駐しているものは各リポジトリで `git fsmonitor--daemon stop` するまで残るので、設定の保存と、実際の本数・空きメモリが減ったことを分けて確かめる。大きなリポジトリだけで使うなら、そこだけローカル設定で有効にする。',
  mcp: 'MCP サーバが AI セッションごとに複製されている。使っていないセッション・エディタを閉じると、親が終了したものから消える。親が終了済みで残っているものは持ち主を確かめてから止める（自動では終了しない）。共有できるサーバは 1 つに寄せる設定を検討する。',
  other: '同じ実行ファイルが多数起動している。親（セッション・アプリ）を確かめ、不要な親を閉じる。個々のプロセスを RSS だけで止めない。',
};
const MIN_RUNAWAY_SEC = 120;
const GPU_ACTIVE_UTIL = 5;
// ps の etime（[[dd-]hh:]mm:ss）を秒に。読めなければ null（tune-core の rules::etime_sec と同じ）
const etimeSec = (s) => {
  const m = /^(?:(?:([0-9]+)-)?([0-9]+):)?([0-9]+):([0-9]+)$/.exec(typeof s === 'string' ? s : '');
  return m ? ((+(m[1] || 0) * 24 + +(m[2] || 0)) * 60 + +m[3]) * 60 + +m[4] : null;
};

const DECISION_STATES = new Set(['observed', 'partial', 'stale', 'unknown']);
const REASON_STATES = new Set(['confirmed', 'hypothesis', 'unknown']);
const PROCESS_CLASSES = new Set(['os_required', 'protected_workload', 'legitimate_parallel', 'duplicate_candidate', 'unclassified']);
const STOP_MIN_CPU = 50;
const MAX_CPU_PERCENT_OF_CORE = 10000;
const safeText = (v) => typeof v === 'string' && v.length > 0 && v.length <= 160 ? v : null;
const safeId = (v) => safeText(v) && /^[A-Za-z0-9_.:-]+$/.test(v) ? v : null;
const safeExeName = (v) => safeText(v) && v.length <= 80 && !/[\\/=]/.test(v) ? v : 'unknown';
const safeStart = (v) => safeText(v) && v.length <= 32 && /^[A-Za-z0-9 :]+$/.test(v) ? v : null;
const safeEvidence = (v) => {
  const sha = safeText(v?.source_sha256);
  if (!sha || !/^[0-9a-f]{64}$/i.test(sha)) return null;
  const run = safeId(v.run); const scope = safeId(v.scope); const rowref = safeId(v.rowref);
  if (!run || !scope || !rowref || !Number.isInteger(v.observed_at)) return null;
  return { run, scope, source_sha256: sha.toLowerCase(), observed_at: v.observed_at, rowref };
};
const safeMetric = (v) => {
  if (!Number.isFinite(v?.value) || v.value < 0 || v.value > MAX_CPU_PERCENT_OF_CORE || v.unit !== 'percent_of_core' || v.definition !== 'cpu_time_delta' || !Number.isInteger(v.sample_window_ms) || v.sample_window_ms <= 0 || v.sample_window_ms > 300000 || !Number.isInteger(v.at)) return null;
  return { value: v.value, unit: v.unit, definition: v.definition, sample_window_ms: v.sample_window_ms, at: v.at };
};
const canAutoFix = (cls, risk, blocked, complete) => cls === 'auto_safe' && risk === 'none' && !blocked && complete;

// gpu_compute は NVIDIA compute-apps の bounded な観測。古い snapshot に項目が
// 無いときは既存の判断を維持し、新しい観測で取得に失敗した場合だけ安全側に倒す。
function gpuProtectsProcess(snap, p) {
  if (p?.gpu_compute === true) return true;
  const compute = snap?.gpu_compute;
  if (!compute || typeof compute !== 'object') return false;
  if (compute.availability === 'absent') return false;
  if (compute.availability !== 'available') return true;
  if (!Array.isArray(compute.apps)) return true;
  if (compute.apps.some((app) => !Number.isInteger(app?.pid) || app.pid <= 0)) return true;
  if (compute.apps.length) return true;
  if (!Array.isArray(snap.gpus) || !snap.gpus.length || snap.gpus.some((gpu) => !Number.isFinite(gpu?.util) || gpu.util < 0 || gpu.util > 100)) return true;
  // util が高いだけでは PID を特定したとは扱わない。機体全体の GPU 活動として
  // 停止候補を保留する（閾値はノイズを除く保守的な 5%）。
  return snap.gpus.some((gpu) => gpu.util >= GPU_ACTIVE_UTIL);
}

// 名前・CPU・同名数では重複を推測しない。collector が明示した根拠だけで停止候補を作る。
function processDecision(p, node, isProtected, now = 0) {
  const meta = p?.decision && typeof p.decision === 'object' ? p.decision : {};
  const evidence_state = DECISION_STATES.has(meta.evidence_state) ? meta.evidence_state : 'unknown';
  const reason_status = REASON_STATES.has(meta.reason_status) ? meta.reason_status : 'unknown';
  const declaredClass = PROCESS_CLASSES.has(meta.process_class) ? meta.process_class : 'unclassified';
  const process_class = isProtected ? 'protected_workload' : declaredClass;
  const stopBlockedByClass = process_class === 'os_required' || process_class === 'protected_workload';
  const identity = {
    pid: Number.isInteger(p?.pid) ? p.pid : 0,
    ppid: Number.isInteger(p?.ppid) ? p.ppid : null,
    started_at: safeStart(p?.start),
    owner_observed: meta.owner_observed === true,
    exe_name: safeExeName(p?.name),
  };
  const evidence = (Array.isArray(meta.evidence) ? meta.evidence : []).map(safeEvidence).filter(Boolean);
  const rawCpuSamples = Array.isArray(meta.cpu_samples) ? meta.cpu_samples : [];
  const metrics = rawCpuSamples.map(safeMetric).filter(Boolean);
  const matchingCpu = rawCpuSamples.length >= 2 && metrics.length === rawCpuSamples.length
    && rawCpuSamples.every((sample) => sample.pid === identity.pid && safeStart(sample.start) === identity.started_at)
    && metrics.every((sample) => sample.value >= STOP_MIN_CPU)
    && metrics.every((m, i) => i === 0 || (metrics[i - 1].at < m.at && metrics[i - 1].sample_window_ms === m.sample_window_ms && m.at - metrics[i - 1].at >= m.sample_window_ms));
  const freshestCpuAt = metrics.at(-1)?.at;
  const freshTimestamps = Number.isInteger(now) && now > 0
    && evidence.every((ref) => ref.observed_at > 0 && ref.observed_at <= now && now - ref.observed_at <= 300000)
    && metrics.every((sample) => sample.at > 0 && sample.at <= now && now - sample.at <= 300000)
    && evidence.every((ref) => freshestCpuAt != null && ref.observed_at >= freshestCpuAt);
  const serviceSchedulerWorkload = meta.service_match === true && meta.scheduler_match === true && meta.workload_match === true;
  const protectReadable = meta.protect_ledger_readable === true;
  const holdClear = node?.hold === false;
  const sharedClear = node?.shared === false;
  const collectorSelfExcluded = meta.collector_self === false;
  const identityComplete = identity.pid > 4 && Number.isInteger(identity.ppid) && identity.ppid > 0 && identity.started_at != null && identity.owner_observed && identity.exe_name !== 'unknown';
  const freshConfirmedDuplicate = evidence_state === 'observed' && reason_status === 'confirmed' && process_class === 'duplicate_candidate';
  const complete = freshConfirmedDuplicate && identityComplete && evidence.length > 0 && matchingCpu && freshTimestamps && serviceSchedulerWorkload && protectReadable && holdClear && sharedClear && collectorSelfExcluded && !stopBlockedByClass;
  const needed_observation = [];
  if (evidence_state !== 'observed') needed_observation.push('fresh_evidence');
  if (reason_status !== 'confirmed') needed_observation.push('confirmed_cause');
  if (process_class !== 'duplicate_candidate') needed_observation.push('service_scheduler_workload_attribution');
  if (!identityComplete) needed_observation.push('pid_name_start_owner_and_parent');
  if (evidence.length === 0) needed_observation.push('evidence_reference');
  if (!matchingCpu) needed_observation.push('two_cpu_time_samples');
  if (!freshTimestamps) needed_observation.push('fresh_timestamp_and_clock_consistency');
  if (!serviceSchedulerWorkload) needed_observation.push('service_scheduler_workload_match');
  if (!protectReadable) needed_observation.push('readable_protect_ledger');
  if (!holdClear) needed_observation.push('explicit_node_hold_clear');
  if (!sharedClear) needed_observation.push('explicit_node_shared_clear');
  if (!collectorSelfExcluded) needed_observation.push('collector_self_excluded');
  if (stopBlockedByClass) needed_observation.push('protected_workload_no_stop');
  const cls = complete ? 'requires_approval' : (stopBlockedByClass ? 'no_action' : (!holdClear || !sharedClear || !protectReadable ? 'manual_only' : 'external_dependency'));
  const risk = complete || stopBlockedByClass || !holdClear || !sharedClear || !protectReadable ? 'high' : 'unknown';
  const proposed_action = complete ? { type: 'kill-process', label: 'このプロセスを終了', params: { pid: identity.pid, name: identity.exe_name, start: identity.started_at, min_cpu: STOP_MIN_CPU } } : null;
  return {
    symptom: 'repeated_high_cpu_samples', evidence_state, process_class, identity, metrics,
    reason: {
      status: reason_status,
      code: complete ? 'duplicate_confirmed' : (stopBlockedByClass ? 'protected_workload' : 'insufficient_stop_evidence'),
      summary: complete ? '二つの CPU 時間差分とサービス・スケジューラ・作業負荷の照合で重複候補を確認した。' : (stopBlockedByClass ? '保護または OS 必須の作業負荷なので、この所見から停止候補は作らない。' : 'CPU と名前だけでは不要なプロセスと断定できないため、停止候補は作らない。'),
    },
    evidence,
    needed_observation,
    remediation: {
      class: cls, stop_risk: risk, can_auto_fix: canAutoFix(cls, risk, !complete, complete),
      alternatives: complete ? ['approval_required_before_stop'] : (stopBlockedByClass ? ['reobserve', 'manual_investigation'] : ['reobserve', 'build_on_another_machine', 'manual_investigation', 'approval_then_restart_candidate']),
    },
    proposed_action,
  };
}

// probe の caches には保存領域も入る。既存 snapshot も同じ完全一致の分類で扱う。
const MAC_CACHE_PATHS = [
  '~/.npm/_cacache', '~/Library/Caches', '~/Library/Caches/Homebrew', '~/.cache', '~/Library/Developer/Xcode/DerivedData',
];
const MAC_STORAGE_PATHS = [
  '~/Library/Developer/CoreSimulator/Devices', '~/.colima', '~/Library/Containers/com.docker.docker/Data',
];
function measuredDirs(snap, paths) {
  const seen = new Set();
  return (snap.caches || []).filter((c) => {
    if (!paths.includes(c?.path) || !Number.isFinite(c.gb) || c.gb < 0 || seen.has(c.path)) return false;
    seen.add(c.path);
    return true;
  });
}

function analyze(snap, node = {}) {
  const out = [];
  const add = (f) => out.push(f);
  const isWin = snap.probe === 'windows';
  const cores = snap.host?.cores || 1;
  const totalMb = (snap.memory?.total_gb || 0) * 1024;
  const procs = snap.processes || {};
  // Windows の cpu は機体全体に対する%（1.5秒の瞬間値）、macOS は1コアに対する%（ps の減衰平均）
  const perCore = (p) => (isWin ? p.cpu * cores : p.cpu);

  // CPU
  if (snap.cpu_busy != null) {
    const top = (procs.apps_cpu || []).slice(0, 3).map((a) => `${a.app} ${Number.isFinite(a.cpu) ? (isWin ? a.cpu * cores : a.cpu).toFixed(0) + '%' : '未取得'}`).join('、');
    if (snap.cpu_busy >= 85) add({ id: 'cpu-saturated', severity: 'critical', category: 'cpu', title: `CPU が飽和している（${snap.cpu_busy}%）`, detail: `上位: ${top}`, advice: '根拠がそろった停止候補だけを確認する。根拠が不足する間は、ほかの重い作業を別の機体へ回す。' });
    else if (snap.cpu_busy >= 60) add({ id: 'cpu-busy', severity: 'warn', category: 'cpu', title: `CPU 使用率が高い（${snap.cpu_busy}%）`, detail: `上位: ${top}`, advice: '常駐アプリの見直しで下がる余地がある。' });
  }

  // 暴走の疑い（1コアを使い切っているプロセス）
  // Windows は瞬間値に加えて、起動からの平均が 1コアの 30% 以上のものだけ（一時的なスパイクを除く）
  // 起動から2分未満のもの（数十秒で終わるバッチなど）は暴走に数えない。経過が分からないものは数える
  const hogs = (procs.top_cpu || []).filter((p) => perCore(p) >= 80 && (!isWin || (p.avg_core ?? 0) >= 30) && (etimeSec(p.etime) ?? Infinity) >= MIN_RUNAWAY_SEC).slice(0, 4);
  for (const p of hogs) {
    const isProtected = node?.protect_gpu === true || NO_KILL.test(p.name) || NO_KILL.test(p.app || '') || gpuProtectsProcess(snap, p);
    const decision = processDecision(p, node, isProtected, Date.now());
    const action = decision.proposed_action || undefined;
    add({
      id: `runaway-${p.name}-${p.pid}`, severity: 'warn', category: 'cpu',
      title: `${p.app && p.app !== p.name ? p.app + ' / ' : ''}${p.name} が CPU ${perCore(p).toFixed(0)}%（1コア換算）`,
      detail: `PID ${p.pid}${p.etime ? `、起動から ${p.etime}` : ''}${isWin ? `（瞬間値。起動からの平均は ${p.avg_core}%）` : '（直近の平均）'}`,
      advice: `${decision.reason.summary} ${action ? '停止には承認が必要。実行時にも保護台帳・PID・名前・起動時刻・CPU を読み直して一致しなければ中止する。' : (adviceFor(p.app, p.name) || (isProtected ? 'OS・セキュリティ、または誰かの作業（AI エージェント・python・node など）の可能性があるため、終了は提案しない。何の処理かを確かめ、原因側（同期・インデックス・ビルド・学習）を止める。' : '所有者・起動時刻・二点の CPU 時間差分・サービス／スケジューラ／作業負荷を再観測する。'))}`,
      decision,
      action,
    });
  }

  // メモリ
  const m = snap.memory || {};
  if (!isWin) {
    if (m.pressure === 'critical') add({ id: 'mem-pressure', severity: 'critical', category: 'memory', title: 'メモリ圧迫が危険域（critical）', detail: `圧縮 ${m.compressed_gb} GB、swap ${m.swap_used_gb}/${m.swap_total_gb} GB`, advice: 'メモリを多く使うアプリから閉じる。下の「メモリの大口」を参照。' });
    else if (m.pressure === 'warn') add({ id: 'mem-pressure', severity: 'warn', category: 'memory', title: 'メモリ圧迫が警告域（warn）', detail: `圧縮 ${m.compressed_gb} GB、swap ${m.swap_used_gb}/${m.swap_total_gb} GB`, advice: 'swap への書き出しで体感が落ちている。メモリの大口を減らすと戻る。' });
    if (m.swap_used_gb >= Math.max(4, (m.total_gb || 0) * 0.15)) add({ id: 'swap', severity: 'warn', category: 'memory', title: `swap を ${m.swap_used_gb} GB 使っている`, detail: `実メモリ ${m.total_gb} GB に対して ${(m.swap_used_gb / m.total_gb * 100).toFixed(0)}%`, advice: '一度大きく溜まった swap はアプリを閉じても残りやすい。大口を閉じたあと、余裕のある時間に再起動すると解消する。' });
  } else {
    if (m.available_pct != null && m.available_pct < 10) add({ id: 'mem-low', severity: 'critical', category: 'memory', title: `空きメモリ ${m.available_pct}%`, detail: `${m.free_gb}/${m.total_gb} GB`, advice: 'メモリの大口を閉じる。' });
    else if (m.available_pct != null && m.available_pct < 20) add({ id: 'mem-low', severity: 'warn', category: 'memory', title: `空きメモリ ${m.available_pct}%`, detail: `${m.free_gb}/${m.total_gb} GB`, advice: 'メモリの大口を閉じる。' });
    if (m.commit_pct >= 90) add({ id: 'commit', severity: 'critical', category: 'memory', title: `コミット済みメモリ ${m.commit_pct}%`, detail: `ページファイル ${m.pagefile_alloc_mb} MB`, advice: '上限に当たるとアプリが落ちる。大口を閉じるか、ページファイルを増やす。' });
    else if (m.commit_pct >= 80) add({ id: 'commit', severity: 'warn', category: 'memory', title: `コミット済みメモリ ${m.commit_pct}%`, detail: `ページファイル ${m.pagefile_alloc_mb} MB（ピーク ${m.pagefile_peak_mb} MB）`, advice: '予約だけで実際には使われていない分も含む。WSL や大きなモデルを動かすとここで詰まる。ページファイルが 4GB 固定なら「システム管理サイズ」に戻すと余裕ができる。' });
  }
  for (const a of (procs.apps || []).filter((a) => totalMb && a.mem_mb >= totalMb * 0.2).slice(0, 3)) {
    add({ id: `mem-hog-${a.app}`, severity: 'warn', category: 'memory', title: `${a.app} が ${gb(a.mem_mb)} GB（実メモリの ${(a.mem_mb / totalMb * 100).toFixed(0)}%）`, detail: `${a.count} プロセスの合計${isWin ? '（ワーキングセット）' : '（RSS。共有分を重ねて数えるため実際より大きめ）'}`, advice: adviceFor(a.app) || 'このアプリの使い方を見直す。' });
  }

  // 同一コマンドの多重起動・孤児（processes.groups）と、プロセス木の根ごとの合計（processes.trees）
  const wsNote = isWin ? '（ワーキングセット）' : '（RSS。共有分を重ねて数えるため実際より大きめ）';
  for (const g of Array.isArray(procs.groups) ? procs.groups : []) {
    if (!g) continue;
    const key = String(g.key);
    const kind = multiKind(key);
    // OS・実行環境の定常的な多重（svchost・launchd 系など）と、システム所有で種類の分からないものは数えない
    if (NO_KILL.test(key) || (g.owner === 'system' && kind === 'other')) continue;
    const th = MULTI[kind];
    const count = +g.count || 0;
    const mem = +g.mem_mb || 0;
    const orphans = +g.orphans || 0;
    const orphanMb = +g.orphan_mb || 0;
    const multi = count >= th.min && mem >= th.mb;
    const orphanOnly = !multi && kind === 'mcp' && orphans >= 2 && orphanMb >= 100;
    if (!multi && !orphanOnly) continue;
    const parent = g.parent ?? '不明';
    const mostlyOrphan = orphans >= 2 && orphans * 2 >= count;
    add({
      id: `${multi ? 'proc-multi' : 'proc-orphan'}-${key}`,
      severity: orphanOnly || (totalMb && mem >= totalMb * 0.1) || count >= 100 ? 'warn' : 'info',
      category: 'memory',
      title: multi ? `${key} が ${count} 本同時に動いている（合計 ${fmtMb(mem)}）` : `${key} の孤児が ${orphans} 本残っている（${fmtMb(orphanMb)}）`,
      detail: `親の多数派: ${parent}、親が終了済み（PPID 1 など）${orphans} 本、所有者 ${g.owner === 'mixed' ? 'ユーザーとシステム' : g.owner === 'system' ? 'システム（Windows はセッション 0・SSH を含む）' : 'ユーザー'}、合計${wsNote}`,
      advice: (kind === 'mcp' && (mostlyOrphan || orphanOnly) ? '半数前後以上が親を失って残っている。セッション終了後の取り残しの可能性が高い。' : '') + MULTI_ADVICE[kind],
    });
  }
  const treeDup = (t) => (procs.apps || []).some((a) => a.app === t.root && totalMb && +a.mem_mb >= totalMb * 0.2);
  for (const t of (Array.isArray(procs.trees) ? procs.trees : []).filter((t) => t && t.owner !== 'system' && totalMb && (+t.count || 0) >= 3 && (+t.mem_mb || 0) >= totalMb * 0.2 && !treeDup(t)).slice(0, 2)) {
    add({
      id: `mem-tree-${t.root}`, severity: 'info', category: 'memory',
      title: `${t.root} を根とするプロセス群が ${gb(t.mem_mb)} GB（実メモリの ${(t.mem_mb / totalMb * 100).toFixed(0)}%、${t.count} プロセス）`,
      detail: `根: ${t.root}（PID ${t.pid}）、直下の子 ${t.children ?? 0} 本、所有者 ${t.owner === 'user' ? 'ユーザー' : String(t.owner)}、群の合計${wsNote}`,
      advice: '単一プロセスの RSS だけで止める判断はしない。根のアプリを作業中でないか確かめてから通常終了する。サービス・コンテナ・待ち受け中のものは、依存先を確かめるまで止めない。',
    });
  }

  // ディスク
  for (const d of snap.disk || []) {
    const sys = d.mount === '/' || /^C:/i.test(d.mount);
    if (d.free_pct < 5 && sys) add({ id: `disk-${d.mount}`, severity: 'critical', category: 'disk', title: `${d.mount} の空きが ${d.free_pct}%（${d.free_gb} GB）`, detail: `容量 ${d.total_gb} GB`, advice: 'システムディスクが埋まると swap とアップデートが止まり、全体が重くなる。容量の内訳と利用状況を確認し、再生成可能と確認できたキャッシュだけ整理を検討する。', commands: cacheCommands(snap) });
    else if (d.free_pct < 10) add({ id: `disk-${d.mount}`, severity: 'warn', category: 'disk', title: `${d.mount} の空きが ${d.free_pct}%（${d.free_gb} GB）`, detail: `容量 ${d.total_gb} GB`, advice: sys ? '10% を切ると swap の確保とアップデートに影響する。整理する前に対象の内容と再生成可否を確認する。' : '大きなファイルの置き場所を見直す。', commands: sys ? cacheCommands(snap) : undefined });
  }
  const measured = isWin ? (snap.caches || []) : measuredDirs(snap, MAC_CACHE_PATHS);
  // 親の有効な計測があれば子は内数。項目しきい値より先に除く（0 も計測済み）。
  const caches = measured.filter((c) => (isWin || !measured.some((p) => c.path.startsWith(p.path + '/'))) && c.gb >= 2);
  const cacheTotal = caches.reduce((s, c) => s + c.gb, 0);
  const cacheUnit = isWin ? 'GB' : 'GiB'; // mac probe の du KiB / 2**20
  if (cacheTotal >= 10) add({ id: 'caches', severity: 'info', category: 'disk', title: `${isWin ? '再生成できるキャッシュ' : '再生成可能なキャッシュ候補'}が ${cacheTotal.toFixed(0)} ${cacheUnit}`, detail: caches.map((c) => `${c.path} ${c.gb} ${cacheUnit}`).join('、'), advice: '既知のキャッシュ領域の計測値であり、削除可能な容量の保証ではない。内容・利用状況・再生成可否を確認してから整理を検討する。ディスクに余裕があるなら急がない。', commands: cacheCommands(snap) });
  if (!isWin) {
    const storage = measuredDirs(snap, MAC_STORAGE_PATHS);
    const storageTotal = storage.reduce((s, c) => s + c.gb, 0);
    if (storageTotal >= 10) add({ id: 'vm-storage', severity: 'info', category: 'disk', title: `VM・シミュレータの保存領域（削除候補ではない）: ${storageTotal.toFixed(0)} GiB`, detail: storage.map((c) => `${c.path} ${c.gb} GiB`).join('、') + '。ユーザーデータを含むため、再生成可能なキャッシュには数えない。', advice: 'VM ディスク・コンテナの永続データ・シミュレータ内のアプリデータを含みうる保存領域。容量の参考情報であり、削除は提案しない。' });
  }

  // 熱・電源
  const pw = snap.power || {};
  if (!isWin) {
    if (pw.cpu_speed_limit != null && pw.cpu_speed_limit < 100) add({ id: 'thermal', severity: 'warn', category: 'thermal', title: `熱で CPU が ${pw.cpu_speed_limit}% に制限されている`, detail: 'pmset -g therm', advice: '通気を確保し、負荷の高い処理を減らす。' });
    if (pw.low_power_mode) add({ id: 'lowpower', severity: 'info', category: 'power', title: '低電力モードが有効', detail: 'pmset lowpowermode 1', advice: '性能を優先するならシステム設定 > バッテリー（または省エネルギー）で低電力モードを切る。' });
  } else {
    if (snap.cpu_perf_pct != null && snap.cpu_perf_pct < 70 && snap.cpu_busy >= 50) add({ id: 'clock-down', severity: 'warn', category: 'thermal', title: `負荷中なのにクロックが定格の ${snap.cpu_perf_pct}%`, detail: `CPU 使用率 ${snap.cpu_busy}%`, advice: '熱か電源設定で絞られている。冷却と電源プランを確認する。' });
    const hasHighPerf = !pw.plans || pw.plans.includes(HIGH_PERF_GUID);
    if (hasHighPerf && pw.plan_guid && [BALANCED_GUID, POWER_SAVER_GUID].includes(pw.plan_guid.toLowerCase())) {
      add({
        id: 'power-plan', severity: pw.plan_guid.toLowerCase() === POWER_SAVER_GUID ? 'warn' : 'info', category: 'power',
        title: `電源プランが「${pw.plan_name}」`, detail: `GUID ${pw.plan_guid}`,
        advice: '1スレッドの速さはほぼ変わらない（2026-10-03 に Windows 機2台で実測して -1.6%、誤差の範囲）。効くのはコアの休止からの復帰遅延くらいで、アイドル時の消費電力は増える。遅延に敏感な処理（音声・ゲーム配信・計測）をする機体だけ試し、効かなければ元に戻す。',
        action: { type: 'set-power-plan', label: '高パフォーマンスにする', params: { guid: HIGH_PERF_GUID, prev_guid: pw.plan_guid } },
      });
    }
    for (const g of snap.gpus || []) {
      if (g.temp_c >= 85) add({ id: `gpu-temp-${g.name}`, severity: 'warn', category: 'thermal', title: `${g.name} が ${g.temp_c}°C`, detail: `使用率 ${g.util}%、${g.power_w}/${g.power_limit_w} W`, advice: 'ファン曲線とケース内の排気を見直す。電力上限を少し下げると温度が大きく下がることが多い。' });
    }
  }

  // 安定性
  const st = snap.stability_7d;
  if (st) {
    const crashes = Math.max(st.bugcheck_1001 || 0, st.kernel_power_41 || 0);
    if (crashes >= 3) add({ id: 'stability', severity: 'critical', category: 'stability', title: `直近7日で予期しない停止が ${crashes} 回`, detail: `BugCheck(1001) ${st.bugcheck_1001}、Kernel-Power(41) ${st.kernel_power_41}、6008 ${st.unexpected_6008}`, advice: '性能より先に安定性の問題。メモリ（XMP を切る・枚数を減らす）と CPU の劣化を疑う。この機体に常駐処理を増やさない。' });
    else if (crashes > 0) add({ id: 'stability', severity: 'warn', category: 'stability', title: `直近7日で予期しない停止が ${crashes} 回`, detail: `BugCheck(1001) ${st.bugcheck_1001}、Kernel-Power(41) ${st.kernel_power_41}`, advice: '停電やスリープ復帰の失敗でも記録される。続くようならダンプを解析する。' });
  }

  // WSL / コンテナ VM
  if (isWin) {
    const vm = (procs.apps || []).find((a) => /^vmmem/i.test(a.app));
    if (vm && totalMb && vm.mem_mb >= totalMb * 0.25 && !snap.wsl?.memory) {
      add({ id: 'wsl-limit', severity: 'warn', category: 'memory', title: `WSL が ${gb(vm.mem_mb)} GB を使っていて上限が無い`, detail: '.wslconfig に memory の指定が無い', advice: 'Windows 側が足りなくなる前に上限を付ける。GPU 計算 PID がある・GPU 使用率が高い・GPU 状態を取得できない、または GPU 学習保護の機体では、`wsl --shutdown` と上限変更の反映を保留する。', commands: [`# %UserProfile%\\.wslconfig\n[wsl2]\nmemory=${Math.max(8, Math.round(snap.memory.total_gb / 2))}GB\n\n[experimental]\nautoMemoryReclaim=gradual`] });
    } else if (!snap.wsl?.memory && snap.wsl) {
      add({ id: 'wsl-limit', severity: 'info', category: 'memory', title: 'WSL のメモリ上限が未設定', detail: '既定では実メモリの半分まで使う', advice: '常用するなら上限と autoMemoryReclaim を設定しておくと、使い終わったメモリが Windows に戻る。GPU 計算 PID がある・GPU 使用率が高い・GPU 状態を取得できない、または GPU 学習保護の機体では、`wsl --shutdown` と上限変更の反映を保留する。', commands: [`# %UserProfile%\\.wslconfig\n[wsl2]\nmemory=${Math.max(8, Math.round((snap.memory?.total_gb || 16) / 2))}GB\n\n[experimental]\nautoMemoryReclaim=gradual`] });
    }
    out.push(...wslInside.findings(snap, { vmMb: vm?.mem_mb ?? null, totalMb }));
    // netsec の防御（lib/netsec.js）があれば、そちらがリアルタイム保護の停止を重大として出す
    if (snap.defender?.realtime === false && !snap.netsec?.defense) add({ id: 'defender-off', severity: 'info', category: 'security', title: 'Defender のリアルタイム保護が無効', detail: '速さは出るが無防備', advice: '性能目的なら全体を切るより、開発ディレクトリだけ除外する方が安全。' });
    const ms = (procs.top_cpu || []).find((p) => p.name === 'MsMpEng');
    if (ms && perCore(ms) >= 30) add({ id: 'defender-cpu', severity: 'warn', category: 'cpu', title: `Defender が CPU ${perCore(ms).toFixed(0)}%（1コア換算）`, detail: `除外 ${snap.defender?.exclusions ?? '不明（管理者権限が要る）'} 件`, advice: adviceFor('MsMpEng'), commands: ['# 管理者 PowerShell で（パスは自分の開発ディレクトリへ）\nAdd-MpPreference -ExclusionPath "$env:USERPROFILE\\ghq"'] });
    if ((snap.startup_items || []).length >= 20) add({ id: 'startup', severity: 'info', category: 'background', title: `スタートアップ項目が ${snap.startup_items.length} 件`, detail: snap.startup_items.slice(0, 12).join('、'), advice: 'タスクマネージャー > スタートアップ で使わないものを無効にする。' });
  } else {
    const c = snap.containers || {};
    for (const v of c.colima || []) {
      if (v.status === 'Running' && m.total_gb && v.memory_gb >= m.total_gb * 0.35) add({ id: `colima-${v.name}`, severity: 'warn', category: 'memory', title: `colima の VM に ${v.memory_gb} GB を割り当てている`, detail: `実メモリ ${m.total_gb} GB、CPU ${v.cpus}`, advice: '割り当てた分はコンテナが使っていなくても macOS から見えにくくなる。必要量まで減らす（VM の再起動が要る）。', commands: [`colima stop ${v.name} && colima start ${v.name} --memory ${Math.max(2, Math.round(m.total_gb / 8))} --cpu ${Math.max(2, Math.round(cores / 3))}`] });
    }
    if (c.docker_desktop && m.total_gb && c.docker_desktop.memory_gb >= m.total_gb * 0.35) add({ id: 'docker-desktop', severity: 'warn', category: 'memory', title: `Docker Desktop に ${c.docker_desktop.memory_gb} GB を割り当てている`, detail: `実メモリ ${m.total_gb} GB`, advice: 'Docker Desktop > Settings > Resources で減らす。' });
    if (snap.time_machine_running) add({ id: 'tm', severity: 'info', category: 'background', title: 'Time Machine のバックアップ中', detail: 'tmutil status', advice: '終わるまで I/O が重い。急ぎの作業中なら一時停止してよい。' });
    // 起動制約（Launch Constraint）で止められた launchd ジョブ。mise・Homebrew で実行ファイルを入れ替えた後に起き、登録し直すと直る
    for (const l of snap.launchd_failing || []) {
      if (!String(l.reason ?? '').includes('CODESIGNING')) continue;
      const job = (snap.jobs || []).find((j) => j.id === l.label);
      const plist = job?.plist && LAUNCH_AGENTS.test(job.plist) && /^[\w.-]+$/.test(l.label) ? job.plist : null;
      add({
        id: `launchd-constraint-${l.label}`, severity: 'warn', category: 'background',
        title: `${l.label} が起動制約で止められている（実行ファイルの更新後）`,
        detail: `終了コード ${l.exit}、${l.reason}`,
        advice: 'launchd に登録し直す（bootout → bootstrap）。mise や Homebrew で python・node を更新したあとに起きる。',
        commands: plist ? [`launchctl bootout gui/$(id -u)/${l.label}\nlaunchctl bootstrap gui/$(id -u) ${plist}`] : undefined,
      });
    }
  }

  // 共通
  if (snap.host?.uptime_h >= 24 * 14) add({ id: 'uptime', severity: 'info', category: 'background', title: `${Math.floor(snap.host.uptime_h / 24)} 日間再起動していない`, detail: '', advice: 'swap・圧縮メモリ・漏れたプロセスは再起動でまとめて片付く。区切りの良いところで。' });
  if ((procs.agent_processes || 0) >= 50) add({ id: 'agents', severity: 'info', category: 'background', title: `AI エージェントのプロセスが ${procs.agent_processes} 個`, detail: 'claude / codex / opencode などの合計', advice: '終わったセッションが残っていないか確認する。1つずつのメモリは小さくても積み上がる。' });
  const b = snap.bench;
  if (b?.runs_ms?.length >= 3) {
    const spread = (Math.max(...b.runs_ms) - Math.min(...b.runs_ms)) / b.median_ms;
    if (spread >= 0.25) add({ id: 'bench-noise', severity: 'info', category: 'cpu', title: `計測のばらつきが ${(spread * 100).toFixed(0)}%`, detail: `${b.runs_ms.join(' / ')} ms`, advice: '裏で断続的に重い処理が走っている。CPU 上位を見て原因を探す。' });
  }

  // ネットワークとセキュリティ（待ち受け・防御・常駐の増減・初めての接続先。lib/netsec.js）
  for (const f of netsec.findings(snap.netsec, node)) add(f);

  // 共用機・不安定機では実行を止めて提案だけにする
  if (node.shared) for (const f of out) if (f.action) { f.action.blocked = '共用機のため、この画面からは実行しない（持ち主と相談）'; }
  return out.sort((a, b) => SEV[a.severity] - SEV[b.severity]);
}

const SEV = { critical: 0, warn: 1, info: 2 };

function cacheCommands(snap) {
  if (snap.probe === 'windows') return ['cleanmgr /sageset:1  # 対象を選ぶ\ncleanmgr /sagerun:1', 'npm cache clean --force'];
  const have = new Set(measuredDirs(snap, MAC_CACHE_PATHS).filter((c) => c.gb >= 1).map((c) => c.path));
  const cmds = [];
  if (have.has('~/Library/Developer/Xcode/DerivedData')) cmds.push('rm -rf ~/Library/Developer/Xcode/DerivedData/*');
  if (have.has('~/.npm/_cacache')) cmds.push('npm cache clean --force');
  if (have.has('~/Library/Caches/Homebrew')) cmds.push('brew cleanup --prune=all');
  cmds.push('du -sh ~/Library/Caches/* 2>/dev/null | sort -h | tail -15   # 大きいものを確認');
  return cmds;
}

function score(findings) {
  const pen = { critical: 25, warn: 8, info: 1 };
  return Math.max(0, 100 - findings.reduce((s, f) => s + pen[f.severity], 0));
}

// 前回との比較。bench が 15% 以上遅くなった／速くなったかを返す
function compare(prev, cur) {
  if (!prev?.bench?.median_ms || !cur?.bench?.median_ms) return null;
  const ratio = cur.bench.median_ms / prev.bench.median_ms;
  return {
    bench_ratio: +ratio.toFixed(3),
    cpu_delta: cur.cpu_busy != null && prev.cpu_busy != null ? +(cur.cpu_busy - prev.cpu_busy).toFixed(1) : null,
    mem_delta: cur.memory?.available_pct != null && prev.memory?.available_pct != null ? +(cur.memory.available_pct - prev.memory.available_pct).toFixed(1) : null,
    verdict: ratio >= 1.15 ? 'slower' : ratio <= 0.87 ? 'faster' : 'same',
  };
}

module.exports = { analyze, score, compare, processDecision, gpuProtectsProcess, canAutoFix, NO_KILL, HIGH_PERF_GUID };
