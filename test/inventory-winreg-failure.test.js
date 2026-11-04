'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { normalizeItems } = require('../lib/inventory');
const { openDb } = require('../lib/db');

function runWinregSection(unreadable) {
  // Run the actual winreg section with simulated registry cmdlets, never the real registry.
  const source = fs.readFileSync(path.join(__dirname, '../probes/win_inventory.ps1'), 'utf8');
  const start = source.indexOf('function Add-Item');
  const end = source.indexOf("Section @('scoop')");
  assert.ok(start >= 0 && end > start);
  const setup = `
$ErrorActionPreference = 'Stop'
$items = New-Object System.Collections.ArrayList
$errors = New-Object System.Collections.ArrayList
$failed = New-Object System.Collections.ArrayList
function Test-Path { return $true }
function Get-ChildItem { [pscustomobject]@{PSPath='visible'}; if (${unreadable ? '$true' : '$false'}) { [pscustomobject]@{PSPath='unreadable'} } }
function Get-ItemProperty { param($LiteralPath) if ($LiteralPath -eq 'unreadable') { throw 'synthetic access denied' }; return [pscustomobject]@{DisplayName='Visible'; DisplayVersion='1'} }
`;
  const script = setup + source.slice(start, end) + `\n[ordered]@{items=@($items);errors=@($errors);failed_sources=@($failed)} | ConvertTo-Json -Compress -Depth 5\n`;
  const executable = path.join(process.env.SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  const child = spawnSync(executable, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], {
    encoding: 'utf8', timeout: 10000, env: { SystemRoot: process.env.SystemRoot, TEMP: os.tmpdir(), TMP: os.tmpdir() }, windowsHide: true,
  });
  assert.equal(child.status, 0, child.stderr);
  return JSON.parse(child.stdout);
}

test('an unreadable registry entry must not remove previously inventoried programs', { skip: process.platform !== 'win32' }, () => {
  const result = runWinregSection(true);
  assert.deepEqual(result.failed_sources, ['winreg']);
  assert.match(result.errors[0], /unreadable/);
  const db = openDb(fs.mkdtempSync(path.join(os.tmpdir(), 'kt-registry-test-')));
  const old = [{source:'winreg', name:'Unreadable', version:'1', explicit:true, extra:null}];
  db.saveInventory('fixture', old, 1000);
  const saved = db.saveInventory('fixture', normalizeItems(result.items), 2000, { skipSources: result.failed_sources });
  assert.equal(saved.removed, 0);
  assert.ok(db.inventory({node_id:'fixture'}).some(row => row.name === 'Unreadable'));
  const complete = runWinregSection(false);
  assert.deepEqual(complete.failed_sources, []);
  const refreshed = db.saveInventory('fixture', normalizeItems(complete.items), 3000, { skipSources: complete.failed_sources });
  assert.equal(refreshed.removed, 1);
  assert.ok(!db.inventory({node_id:'fixture'}).some(row => row.name === 'Unreadable'));
});
