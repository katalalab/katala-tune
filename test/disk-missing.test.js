'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
const Live = require('../renderer/live');

test('Disk missing counters, partial coverage and recovery keep Python rates unknown', (t) => {
  const python = [process.env.PYTHON, 'python3', 'python'].filter(Boolean)
    .find(p => spawnSync(p, ['--version'], { encoding: 'utf8', timeout: 5000 }).status === 0);
  if (!python) return t.skip('Python unavailable');
  const result = spawnSync(python, ['-I', path.join(__dirname, 'fixtures/disk-missing.py')], { encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test('Disk fixtures read the UTF-8 probe when the OS default encoding is cp1252', (t) => {
  const python = [process.env.PYTHON, 'python3', 'python'].filter(Boolean)
    .find(p => spawnSync(p, ['--version'], { encoding: 'utf8', timeout: 5000 }).status === 0);
  if (!python) return t.skip('Python unavailable');
  // Only replace the environment's default; an explicit file encoding still wins.
  const boot = `import pathlib,runpy,sys
read = pathlib.Path.read_text
def legacy_default(self,*args,**kwargs):
    if not args and kwargs.get('encoding') is None: kwargs['encoding']='cp1252'
    return read(self,*args,**kwargs)
pathlib.Path.read_text=legacy_default
sys.argv=[sys.argv[1]]
runpy.run_path(sys.argv[0],run_name='__main__')`;
  const result = spawnSync(python, ['-I', '-c', boot, path.join(__dirname, 'fixtures/disk-missing.py')], { encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test('Disk missing display differs from an observed idle disk', () => {
  assert.equal(Live.rate(null), '–');
  assert.equal(Live.rate(undefined), '–');
  assert.equal(Live.rate(0), '0 B/s');
  assert.equal(Live.rate(120), '120 B/s');
});

test('a missing Disk point clears the current rate after a valid point', () => {
  const store = {};
  const send = points => Live.merge(store, { nodes: { fixture: { points } } }, 3000);
  send([{t:1000,dr:120,dw:240}]);
  send([{t:2000,dw:60}]);
  assert.equal(Live.rate(store.fixture.points.at(-1).dr), '–');
  assert.equal(Live.rate(store.fixture.points.at(-1).dw), '60 B/s');
  send([{t:3000,dr:0,dw:0}]);
  assert.equal(Live.rate(store.fixture.points.at(-1).dr), '0 B/s');
});
