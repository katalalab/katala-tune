// snapshot（probe の出力）から所見と提案を作る。副作用なし。
// finding = { id, severity: critical|warn|info, category, title, detail, advice, commands?, action? }
'use strict';

const HIGH_PERF_GUID = '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c';
const BALANCED_GUID = '381b4222-f694-41f0-9685-ff5bb260df2e';
const POWER_SAVER_GUID = 'a1841308-3541-4fab-bc81-f71556f20b4a';

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
  [/^vmmemWSL$|^vmmem$/, 'WSL の VM。使い終わったら `wsl --shutdown` でメモリを返す。常時大きいなら %UserProfile%\\.wslconfig の memory で上限を付ける。'],
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
    const top = (procs.apps_cpu || []).slice(0, 3).map((a) => `${a.app} ${isWin ? (a.cpu * cores).toFixed(0) : a.cpu.toFixed(0)}%`).join('、');
    if (snap.cpu_busy >= 85) add({ id: 'cpu-saturated', severity: 'critical', category: 'cpu', title: `CPU が飽和している（${snap.cpu_busy}%）`, detail: `上位: ${top}`, advice: '下の「暴走の疑い」から順に止める。止められない処理なら、その間ほかの重い作業を別の機体へ回す。' });
    else if (snap.cpu_busy >= 60) add({ id: 'cpu-busy', severity: 'warn', category: 'cpu', title: `CPU 使用率が高い（${snap.cpu_busy}%）`, detail: `上位: ${top}`, advice: '常駐アプリの見直しで下がる余地がある。' });
  }

  // 暴走の疑い（1コアを使い切っているプロセス）
  // Windows は瞬間値に加えて、起動からの平均が 1コアの 30% 以上のものだけ（一時的なスパイクを除く）
  const hogs = (procs.top_cpu || []).filter((p) => perCore(p) >= 80 && (!isWin || (p.avg_core ?? 0) >= 30)).slice(0, 4);
  for (const p of hogs) {
    const killable = !NO_KILL.test(p.name) && !NO_KILL.test(p.app || '');
    add({
      id: `runaway-${p.name}-${p.pid}`, severity: 'warn', category: 'cpu',
      title: `${p.app && p.app !== p.name ? p.app + ' / ' : ''}${p.name} が CPU ${perCore(p).toFixed(0)}%（1コア換算）`,
      detail: `PID ${p.pid}${p.etime ? `、起動から ${p.etime}` : ''}${isWin ? `（瞬間値。起動からの平均は ${p.avg_core}%）` : '（直近の平均）'}`,
      advice: adviceFor(p.app, p.name) || (killable ? '想定外ならアプリを終了して様子を見る。' : 'OS・セキュリティ、または誰かの作業（AI エージェント・python・node など）の可能性があるため、終了は提案しない。何の処理かを確かめ、原因側（同期・インデックス・ビルド・学習）を止める。'),
      // min_cpu: 終了の直前に測り直して、これを下回っていたら（回復していたら）中止する
      action: killable ? { type: 'kill-process', label: 'このプロセスを終了', params: { pid: p.pid, name: p.name, start: p.start ?? null, min_cpu: 50 } } : undefined,
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

  // ディスク
  for (const d of snap.disk || []) {
    const sys = d.mount === '/' || /^C:/i.test(d.mount);
    if (d.free_pct < 5 && sys) add({ id: `disk-${d.mount}`, severity: 'critical', category: 'disk', title: `${d.mount} の空きが ${d.free_pct}%（${d.free_gb} GB）`, detail: `容量 ${d.total_gb} GB`, advice: 'システムディスクが埋まると swap とアップデートが止まり、全体が重くなる。下のキャッシュ削除コマンドから始める。', commands: cacheCommands(snap) });
    else if (d.free_pct < 10) add({ id: `disk-${d.mount}`, severity: 'warn', category: 'disk', title: `${d.mount} の空きが ${d.free_pct}%（${d.free_gb} GB）`, detail: `容量 ${d.total_gb} GB`, advice: sys ? '10% を切ると swap の確保とアップデートに影響する。' : '大きなファイルの置き場所を見直す。', commands: sys ? cacheCommands(snap) : undefined });
  }
  const caches = (snap.caches || []).filter((c) => c.gb >= 2);
  const cacheTotal = caches.reduce((s, c) => s + c.gb, 0);
  if (cacheTotal >= 10) add({ id: 'caches', severity: 'info', category: 'disk', title: `再生成できるキャッシュが ${cacheTotal.toFixed(0)} GB`, detail: caches.map((c) => `${c.path} ${c.gb} GB`).join('、'), advice: '消しても次に使うとき作り直される。ディスクに余裕があるなら急がない。', commands: cacheCommands(snap) });

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
      add({ id: 'wsl-limit', severity: 'warn', category: 'memory', title: `WSL が ${gb(vm.mem_mb)} GB を使っていて上限が無い`, detail: '.wslconfig に memory の指定が無い', advice: 'Windows 側が足りなくなる前に上限を付ける。反映には `wsl --shutdown` が要る。', commands: [`# %UserProfile%\\.wslconfig\n[wsl2]\nmemory=${Math.max(8, Math.round(snap.memory.total_gb / 2))}GB\n\n[experimental]\nautoMemoryReclaim=gradual`] });
    } else if (!snap.wsl?.memory && snap.wsl) {
      add({ id: 'wsl-limit', severity: 'info', category: 'memory', title: 'WSL のメモリ上限が未設定', detail: '既定では実メモリの半分まで使う', advice: '常用するなら上限と autoMemoryReclaim を設定しておくと、使い終わったメモリが Windows に戻る。', commands: [`# %UserProfile%\\.wslconfig\n[wsl2]\nmemory=${Math.max(8, Math.round((snap.memory?.total_gb || 16) / 2))}GB\n\n[experimental]\nautoMemoryReclaim=gradual`] });
    }
    if (snap.defender?.realtime === false) add({ id: 'defender-off', severity: 'info', category: 'security', title: 'Defender のリアルタイム保護が無効', detail: '速さは出るが無防備', advice: '性能目的なら全体を切るより、開発ディレクトリだけ除外する方が安全。' });
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
  }

  // 共通
  if (snap.host?.uptime_h >= 24 * 14) add({ id: 'uptime', severity: 'info', category: 'background', title: `${Math.floor(snap.host.uptime_h / 24)} 日間再起動していない`, detail: '', advice: 'swap・圧縮メモリ・漏れたプロセスは再起動でまとめて片付く。区切りの良いところで。' });
  if ((procs.agent_processes || 0) >= 50) add({ id: 'agents', severity: 'info', category: 'background', title: `AI エージェントのプロセスが ${procs.agent_processes} 個`, detail: 'claude / codex / opencode などの合計', advice: '終わったセッションが残っていないか確認する。1つずつのメモリは小さくても積み上がる。' });
  const b = snap.bench;
  if (b?.runs_ms?.length >= 3) {
    const spread = (Math.max(...b.runs_ms) - Math.min(...b.runs_ms)) / b.median_ms;
    if (spread >= 0.25) add({ id: 'bench-noise', severity: 'info', category: 'cpu', title: `計測のばらつきが ${(spread * 100).toFixed(0)}%`, detail: `${b.runs_ms.join(' / ')} ms`, advice: '裏で断続的に重い処理が走っている。CPU 上位を見て原因を探す。' });
  }

  // 共用機・不安定機では実行を止めて提案だけにする
  if (node.shared) for (const f of out) if (f.action) { f.action.blocked = '共用機のため、この画面からは実行しない（持ち主と相談）'; }
  return out.sort((a, b) => SEV[a.severity] - SEV[b.severity]);
}

const SEV = { critical: 0, warn: 1, info: 2 };

function cacheCommands(snap) {
  if (snap.probe === 'windows') return ['cleanmgr /sageset:1  # 対象を選ぶ\ncleanmgr /sagerun:1', 'npm cache clean --force'];
  const have = new Set((snap.caches || []).filter((c) => c.gb >= 1).map((c) => c.path));
  const cmds = [];
  if (have.has('~/Library/Developer/CoreSimulator/Devices')) cmds.push('xcrun simctl delete unavailable');
  if (have.has('~/Library/Developer/Xcode/DerivedData')) cmds.push('rm -rf ~/Library/Developer/Xcode/DerivedData/*');
  if (have.has('~/.npm/_cacache')) cmds.push('npm cache clean --force');
  if (have.has('~/Library/Caches/Homebrew')) cmds.push('brew cleanup --prune=all');
  if (have.has('~/.colima') || snap.containers?.colima?.length) cmds.push('docker system df   # 確認してから\ndocker image prune -a');
  if (snap.containers?.docker_desktop) cmds.push('docker system df   # 確認してから\ndocker builder prune');
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

module.exports = { analyze, score, compare, NO_KILL, HIGH_PERF_GUID };
