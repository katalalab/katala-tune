// Dedicated transport parity. Synthetic input only; no process/SSH may run.
'use strict';
const fs = require('node:fs');
const transportModule = require('../../../../lib/windows-log-transport');
transportModule.runWindowsLogs = async () => { throw Error('Real transport forbidden in parity'); };
const logs = require('../../../../lib/logs');
const collect = require('../../../../lib/collect');
const cases = JSON.parse(fs.readFileSync(0, 'utf8'));
const sourceNames = ['win_system', 'win_application', 'neonmonitor'];
const node = { id: 'example-node', alias: 'example-node', os: 'windows', network: false };
const valid = JSON.stringify({ sources: Object.fromEntries(sourceNames.map(s => [s, { cursor: '10', rows: [] }])) });
const resultOf = c => ({ code: c.code, out: c.output === 'valid' ? valid : c.output === 'partial' ? '{"sources":' : '', err: c.err });
(async () => {
  const transport = cases.transport.map(c => {
    try { return (c.kind === 'events' ? logs.windowsRemoteTransport : logs.windowsLogonsTransport)(c.cursors, Buffer.from(c.script.repeat(c.repeat ?? 1))); }
    catch (e) { return { error: e.message }; }
  });
  const results = [];
  for (const c of cases.results) {
    const state = Object.fromEntries(sourceNames.map(s => [s, { cursor: '5', error: null }]));
    const db = { cursor: (_n, s) => state[s], insertLogs: () => 0,
      cursorOk: (_n, s, cursor) => { state[s] = { cursor, error: null }; },
      cursorError: (_n, s, error) => { state[s].error = error; } };
    const summary = await logs.syncNode(db, node, { runWindows: async () => resultOf(c) });
    results.push({ error: summary.error ?? null, state });
  }
  const main = { code: 0, out: valid, err: '' };
  const security = [];
  for (const c of cases.results) {
    const r = resultOf(c);
    if (c.output === 'valid') r.out = JSON.stringify({ sources: { win_security: { cursor: '9', rows: [] } } });
    const merged = collect.lastJsonLine(logs.mergeLogons(main, r).out);
    const state = Object.fromEntries([...sourceNames, 'win_security'].map(s => [s, { cursor: '5', error: null }]));
    const db = { cursor: (_n, s) => state[s], insertLogs: () => 0,
      cursorOk: (_n, s, cursor) => { state[s] = { cursor, error: null }; },
      cursorError: (_n, s, error) => { state[s].error = error; } };
    await logs.syncNode(db, { ...node, network: true }, { runWindows: async (_cmd, _args, t) => t.input.startsWith('KTLOG1\nlogons\n') ? r : main });
    security.push({ data: merged, state: { win_system: state.win_system, win_security: state.win_security } });
  }
  process.stdout.write(JSON.stringify({ transport, results, security }));
})().catch(() => { process.stderr.write('Log transport parity evaluation failed\n'); process.exitCode = 1; });
