'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { openDb } = require('../lib/db');
test('lifecycle schema survives legacy reopen and does not enter OS log FTS', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'monitor-evidence-'));
  try {
    let store = openDb(dir);
    store.setMeta('fixture', { held: true });
    const columns = store.db.prepare('pragma table_info(monitor_events)').all().map(x => x.name);
    assert.deepEqual(columns, ['id','collector_id','event_seq','node_id','ts','payload']);
    store.db.prepare('insert into monitor_events(collector_id,event_seq,node_id,ts,payload) values(?,?,?,?,?)').run('fixture',1,'node',1,'{}');
    assert.throws(() => store.db.prepare('insert into monitor_events(collector_id,event_seq,node_id,ts,payload) values(?,?,?,?,?)').run('fixture',2,'node',1,'x'.repeat(1025)), /CHECK/);
    store.db.close();
    store = openDb(dir);
    assert.deepEqual(store.getMeta('fixture'), { held:true });
    assert.equal(store.db.prepare('select count(*) n from monitor_events').get().n,1);
    assert.equal(store.db.prepare('select count(*) n from logs').get().n,0);
    assert.equal(store.db.prepare('select count(*) n from logs_fts').get().n,0);
    store.db.close();
  } finally { fs.rmSync(dir, {recursive:true,force:true}); }
});
