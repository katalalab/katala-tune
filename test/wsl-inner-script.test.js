'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

// Run the actual fixed shell payload with fixture commands, never wsl.exe or /proc reads.
const source = fs.readFileSync(path.join(__dirname, '../probes/win_probe.ps1'), 'utf8');
const script = source.split("$KtWslInnerScript = @'\n")[1].split("\n'@")[0];
const fixtures = `
grep() { printf 'MemTotal: 4194304 kB\\nMemAvailable: 2097152 kB\\nSwapTotal: 0 kB\\nSwapFree: 0 kB\\n'; }
cat() { return 1; }
ps() { i=1; while [ "$i" -le 12 ]; do printf ' %s 1024 0.0 fixture-process-%s\\n' "$i" "$i"; i=$((i+1)); done; return "$PS_CODE"; }
df() { printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n/dev/fixture 100 20 80 20%% /\\n'; return "$DF_CODE"; }
`;

for (const [psCode, dfCode] of [[0, 0], [127, 1]]) {
  test(`inside payload preserves ps/df exit status ${psCode}/${dfCode} after output limiting`, { skip: process.platform === 'win32' }, () => {
    const r = spawnSync('/bin/sh', ['-s'], { input: fixtures + script, encoding: 'utf8', timeout: 3000, env: { ...process.env, PS_CODE: String(psCode), DF_CODE: String(dfCode) } });
    assert.ifError(r.error);
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /MemTotal: 4194304/);
    assert.match(r.stdout, /fixture-process-8\n/);
    assert.doesNotMatch(r.stdout, /fixture-process-9\n/);
    assert.match(r.stdout, new RegExp(`@@status ps ${psCode}\\n`));
    assert.match(r.stdout, new RegExp(`@@status df ${dfCode}\\n`));
    assert.match(r.stdout, /@@status psi 1\n/);
    assert.match(r.stdout, /@@status load 1\n/);
  });
}
