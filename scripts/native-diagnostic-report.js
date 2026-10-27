'use strict';
const fs = require('node:fs');
const observation = /^native_role=(?:topology-native-reader|topology-supervise|topology-verify) elapsed_ms=\d{1,8} code=(?:Some\(\d{1,3}\)|None) stdout_len=\d{1,9} stderr_len=\d{1,9}$/;
function decode(buffer) {
  return buffer[0] === 0xff && buffer[1] === 0xfe ? buffer.subarray(2).toString('utf16le') : buffer.toString('utf8');
}
function report(text) {
  return text.split(/\r?\n/).filter(line => observation.test(line)).slice(0, 3);
}
if (require.main === module) {
  try {
    const lines = report(decode(fs.readFileSync(process.argv[2])));
    console.log(lines.length ? lines.join('\n') : 'No closed native role observations were captured.');
  } catch {
    console.error('Native diagnostic log could not be read.');
    process.exitCode = 1;
  }
}
module.exports = { report, decode };
