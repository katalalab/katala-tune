'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { openDb } = require('../../../../lib/db');

module.exports = function withTempDb(prefix, run) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  let store;
  try {
    store = openDb(dir);
    return run(store);
  } finally {
    try {
      if (store) store.db.close();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }
};
