'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { openDb } = require('../lib/db');
const { normalizeItems } = require('../lib/inventory');

// Run the actual macOS probe with a synthetic filesystem; never read installed apps.
function runProbe(denied) {
  const python = [process.env.PYTHON, 'python3', 'python'].filter(Boolean)
    .find(cmd => spawnSync(cmd, ['--version'], { encoding: 'utf8', timeout: 5000 }).status === 0);
  assert.ok(python, 'Python 3 is required for the inventory probe test');
  const boot = `
import contextlib, io, json, os, sys
from unittest.mock import patch
source = open(sys.argv[1], encoding='utf-8-sig').read()
home = os.path.join('synthetic', 'home')
node = os.path.join(home, '.local', 'share', 'mise', 'installs', 'node')
denied = {'app': '/Applications', 'npm': node}.get(sys.argv[2])
def listing(p):
    if denied and os.path.normpath(os.fspath(p)) == os.path.normpath(denied):
        raise PermissionError('synthetic denied')
    if os.path.normpath(os.fspath(p)) == os.path.normpath('/Applications/Utilities'):
        raise FileNotFoundError('synthetic missing')
    entries = {
        os.path.normpath('/Applications'): ['Visible.app', '.hidden.app', 'notes.txt'],
        os.path.normpath(node): ['20.0'],
        os.path.normpath(os.path.join(node, '20.0', 'lib', 'node_modules')): ['demo'],
    }
    if sys.argv[2] in ('brew_version', 'brew_present'):
        entries[os.path.normpath('/opt/homebrew/Cellar')] = ['demo']
        entries[os.path.normpath('/opt/homebrew/Cellar/demo')] = ['1.0']
        return entries.get(os.path.normpath(os.fspath(p)), [])
    if sys.argv[2] == 'none': return entries.get(os.path.normpath(os.fspath(p)), [])
    return []
class EmptyScan:
    def __enter__(self): return iter(())
    def __exit__(self, *args): return False
def scanning(p):
    if denied and os.path.normpath(os.fspath(p)) == os.path.normpath(denied):
        raise PermissionError('synthetic denied')
    return EmptyScan()
def stat(p, *args, **kwargs):
    if sys.argv[2] == 'brew_present' and os.path.normpath(os.fspath(p)) == os.path.normpath('/opt/homebrew/Cellar/demo/1.0'):
        return os.stat_result((0o040755, 0, 0, 1, 0, 0, 0, 0, 1, 0))
    if sys.argv[2] == 'brew_version' and os.path.normpath(os.fspath(p)) == os.path.normpath('/opt/homebrew/Cellar/demo/1.0'):
        raise PermissionError('synthetic denied')
    if os.path.normpath(os.fspath(p)) == os.path.normpath(os.path.join(home, '.cargo', 'bin', 'rustup')):
        if sys.argv[2] == 'platform':
            raise PermissionError('synthetic denied')
        if sys.argv[2] == 'platform_present':
            return os.stat_result((0,) * 10)
    raise FileNotFoundError('synthetic missing')
env = {'HOME': home, 'USERPROFILE': home, 'MISE_DATA_DIR': os.path.join(home, '.local', 'share', 'mise')}
out = io.StringIO()
with patch.dict(os.environ, env, clear=True), patch('os.listdir', listing), patch('os.scandir', scanning), patch('os.stat', stat), patch('os.path.realpath', side_effect=lambda p: p), patch('builtins.open', side_effect=FileNotFoundError()), contextlib.redirect_stdout(out):
    exec(compile(source, sys.argv[1], 'exec'), {})
print(out.getvalue(), end='')
`;
  const result = spawnSync(python, ['-I', '-c', boot, path.join(__dirname, '../probes/mac_inventory.py'), denied], {
    encoding: 'utf8', timeout: 10000, windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

test('macOS readable and missing roots report available apps and npm packages', () => {
  const result = runProbe('none');
  assert.deepEqual(result.failed_sources, []);
  assert.ok(result.items.some(item => item.source === 'app' && item.name === 'Visible'));
  assert.ok(result.items.some(item => item.source === 'npm' && item.name === 'demo'));
  assert.ok(!result.items.some(item => item.name === '.hidden'));
});

test('macOS installed platform is reported', () => {
  const result = runProbe('platform_present');
  assert.deepEqual(result.failed_sources, []);
  assert.ok(result.items.some(item => item.source === 'platform' && item.name === 'rustup'));
});

test('macOS readable Homebrew version remains available', () => {
  const result = runProbe('brew_present');
  assert.deepEqual(result.failed_sources, []);
  assert.ok(result.items.some(item => item.source === 'brew' && item.name === 'demo' && item.version === '1.0'));
});

for (const source of ['app', 'npm', 'platform', 'brew']) {
  test(`macOS ${source} access denial must protect previous inventory`, () => {
    const result = runProbe(source === 'brew' ? 'brew_version' : source);
    assert.ok(result.failed_sources.includes(source), `missing ${source} failure: ${JSON.stringify(result)}`);
    assert.ok(result.errors.length > 0);
    const db = openDb(fs.mkdtempSync(path.join(os.tmpdir(), 'kt-macos-inventory-')));
    db.saveInventory('fixture', [{ source, name: 'PreviouslySeen', version: '1', explicit: true, extra: null }], 1000);
    const saved = db.saveInventory('fixture', normalizeItems(result.items), 2000, { skipSources: result.failed_sources });
    assert.equal(saved.removed, 0);
    assert.ok(db.inventory({ node_id: 'fixture' }).some(item => item.name === 'PreviouslySeen' && item.version === '1'));
  });
}
