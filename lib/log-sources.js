'use strict';
const netsec = require('./netsec');

const SOURCES = { macos: ['mac_diag', 'mac_kernel', 'mac_auth'], windows: ['win_system', 'win_application', 'neonmonitor', 'win_security'] };
const LOGIN_SOURCES = ['mac_auth', 'win_security'];
const sourceEnabled = (node, source) => netsec.loginsEnabled(node) || !LOGIN_SOURCES.includes(source);
const sourcesOf = (node) => (SOURCES[node.os] || []).filter((source) => sourceEnabled(node, source));

module.exports = { SOURCES, LOGIN_SOURCES, sourceEnabled, sourcesOf };
