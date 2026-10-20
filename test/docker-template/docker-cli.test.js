'use strict';
// Explicit Linux CI gate (also runnable locally); no daemon or Docker SDK.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const { once } = require('node:events');
const { execFile } = require('node:child_process');
const ID = 'a'.repeat(64);
const source = fs.readFileSync(path.join(__dirname, '..', '..', 'crates', 'tune-core', 'src', 'topology', 'adapters', 'docker.rs'), 'utf8');
const format = source.match(/const CONTAINER_FORMAT: &str = r#"([^]*?)"#;/)?.[1];
assert.ok(format, 'read the exact production raw template');
function inspectFixture() {
  return {
    Id: ID, State: { Status: 'running', Health: { Status: 'healthy', Log: [{ Output: 'DO_NOT_EXPORT' }] } },
    HostConfig: { Memory: 0, NanoCpus: 0, CpuPeriod: 0, CpuQuota: 0, CpuShares: 0, CpusetCpus: '', RestartPolicy: { Name: 'no', MaximumRetryCount: 0 }, LogConfig: { Type: 'json-file', Config: {} } },
    Mounts: [], NetworkSettings: { Networks: {}, Ports: {} },
    Config: { Env: ['DO_NOT_EXPORT'], Cmd: ['DO_NOT_EXPORT'], Labels: { secret: 'DO_NOT_EXPORT' } },
  };
}
async function render(fixture, signal, stopOnInspect = null) {
  signal.throwIfAborted();
  const config = fs.mkdtempSync(path.join(os.tmpdir(), 'docker-template-'));
  const requests = [], violations = [];
  let child, childClosed = false, result, resolveClose;
  // Unlike events.once(child, 'close'), this promise never rejects on the error event.
  const closed = new Promise(resolve => { resolveClose = resolve; });
  const waitClosed = milliseconds => new Promise(resolve => {
    const timer = setTimeout(() => resolve(false), milliseconds);
    closed.then(() => { clearTimeout(timer); resolve(true); });
  });
  const abort = new AbortController();
  signal = AbortSignal.any([signal, abort.signal]);
  const server = http.createServer((req, res) => {
    requests.push({ method: req.method, path: req.url });
    res.setHeader('Connection', 'close');
    if (requests.length > 8 || !['GET', 'HEAD'].includes(req.method)) {
      violations.push('request budget/method'); res.writeHead(403).end(); return;
    }
    if (req.url === '/_ping') {
      res.setHeader('API-Version', '1.48'); res.setHeader('OSType', 'linux'); res.end(req.method === 'HEAD' ? undefined : 'OK'); return;
    }
    if (req.method === 'GET' && req.url === `/v1.48/containers/${ID}/json`) {
      if (stopOnInspect === 'kill') { child.kill('SIGKILL'); return; }
      if (stopOnInspect === 'abort') { abort.abort(); return; }
      res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(fixture)); return;
    }
    violations.push('unexpected endpoint'); res.writeHead(404).end();
  });
  server.requestTimeout = 1000; server.headersTimeout = 1000;
  const expiry = setTimeout(() => server.closeAllConnections(), 10000);
  try {
    server.listen(0, '127.0.0.1'); await once(server, 'listening', { signal });
    signal.throwIfAborted();
    const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^DOCKER_/i.test(key) && !/^(?:http|https|all|no)_proxy$/i.test(key)));
    result = await new Promise(resolve => {
      child = execFile('docker', ['--config', config, '--host', `tcp://127.0.0.1:${server.address().port}`, 'container', 'inspect', '--format', format, ID],
        { env, signal, shell: false, timeout: 8000, killSignal: 'SIGKILL', maxBuffer: 65536, encoding: 'utf8', windowsHide: true },
        (error, stdout, stderr) => resolve({ code: error ? (error.code ?? 'process-failed') : 0, stdout, stderr }));
      child.once('close', () => { childClosed = true; resolveClose(); });
    });
    assert.deepEqual(violations, [], 'only the fixed read-only loopback API may be called');
    assert.ok(requests.some(r => r.method === 'GET' && r.path === `/v1.48/containers/${ID}/json`), 'inspect reached the fixture');
    assert.ok(requests.every(r => ['GET', 'HEAD'].includes(r.method)));
    return result;
  } finally {
    clearTimeout(expiry);
    let cleanupError;
    if (child && !childClosed && !await waitClosed(2000)) {
      child.kill('SIGKILL'); server.closeAllConnections();
      if (!await waitClosed(1000)) cleanupError = new Error('owned Docker CLI close unconfirmed; private config retained');
    }
    if (result) result.closeObservedBeforeCleanup = childClosed;
    server.closeAllConnections();
    if (server.listening) await new Promise(resolve => server.close(resolve));
    if (!child || childClosed) fs.rmSync(config, { recursive: true, force: true });
    if (cleanupError) throw cleanupError;
  }
}
async function projected(fixture, signal) {
  const out = await render(fixture, signal);
  assert.equal(out.code, 0, `real Docker Go-template failed: ${out.stderr}`);
  assert.equal(out.stderr, '');
  assert.equal(out.stdout.includes('DO_NOT_EXPORT'), false, 'private config and health logs must not be selected');
  return JSON.parse(out.stdout);
}
test('missing optional Health renders null through real Docker template evaluation', { timeout: 15000 }, async t => {
  const fixture = inspectFixture(); delete fixture.State.Health;
  const row = await projected(fixture, t.signal);
  assert.equal(row.health, null); assert.equal(row.memory, 0); assert.equal(row.nano_cpus, 0);
});
test('bind mount without optional Name renders null without losing readonly false', { timeout: 15000 }, async t => {
  const fixture = inspectFixture(); fixture.Mounts = [{ Type: 'bind', Source: '/fixture/source', Destination: '/fixture/target', RW: false }];
  const row = await projected(fixture, t.signal);
  assert.deepEqual(row.mounts, [{ kind: 'bind', source: '/fixture/source', target: '/fixture/target', rw: false, name: null }, null]);
});
test('healthy/unhealthy statuses and volume identity survive exact template projection', { timeout: 15000 }, async t => {
  for (const status of ['healthy', 'unhealthy']) {
    const fixture = inspectFixture(); fixture.State.Health.Status = status;
    fixture.State.Status = status === 'unhealthy' ? 'exited' : 'running';
    fixture.Mounts = [{ Type: 'volume', Name: 'fixture-volume', Source: '/fixture/volume', Destination: '/volume', RW: true }];
    const row = await projected(fixture, t.signal);
    assert.equal(row.health, status); assert.equal(row.state, fixture.State.Status);
    assert.equal(row.mounts[0].name, 'fixture-volume'); assert.equal(row.mounts[0].rw, true);
  }
});
test('nullable and empty log options remain unknown while explicit zero remains zero text', { timeout: 15000 }, async t => {
  for (const config of [null, {}, { 'max-size': '0', 'max-file': '0' }]) {
    const fixture = inspectFixture(); fixture.HostConfig.LogConfig.Config = config;
    const row = await projected(fixture, t.signal);
    assert.equal(row.max_size, config?.['max-size'] ?? null);
    assert.equal(row.max_file, config?.['max-file'] ?? null);
  }
});
test('health object without Status and explicit null remain unknown', { timeout: 15000 }, async t => {
  for (const health of [null, {}, { FailingStreak: 0 }]) {
    const fixture = inspectFixture(); fixture.State.Health = health;
    assert.equal((await projected(fixture, t.signal)).health, null);
  }
});
test('nullable resource values are distinct from explicit zero', { timeout: 15000 }, async t => {
  for (const value of [null, 0]) {
    const fixture = inspectFixture(); fixture.HostConfig.Memory = value; fixture.HostConfig.NanoCpus = value;
    const row = await projected(fixture, t.signal);
    assert.equal(row.memory, value); assert.equal(row.nano_cpus, value);
  }
});
test('malformed health and log-option types fail instead of becoming blank successful JSON', { timeout: 15000 }, async t => {
  for (const field of ['health', 'config']) {
    const fixture = inspectFixture();
    if (field === 'health') fixture.State.Health = 'invalid'; else fixture.HostConfig.LogConfig.Config = 'invalid';
    const result = await render(fixture, t.signal);
    assert.notEqual(result.code, 0); assert.notEqual(result.stderr, '');
  }
});

test('signal-terminated Docker CLI is a failure after it reaches the fixture', { timeout: 15000 }, async t => {
  const result = await render(inspectFixture(), t.signal, 'kill');
  assert.notEqual(result.code, 0, 'a killed CLI must never be normalized to success');
});

test('aborted Docker CLI closes before fixture cleanup completes', { timeout: 15000 }, async t => {
  const result = await render(inspectFixture(), t.signal, 'abort');
  assert.notEqual(result.code, 0);
  assert.equal(result.closeObservedBeforeCleanup, true, 'AbortError callback alone must not authorize cleanup');
});
