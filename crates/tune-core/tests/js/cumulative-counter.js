'use strict';
const { step } = require('../../../../lib/cumulative-counter');
const fixture = require('../../../../test/fixtures/cumulative-counter.json');
const output = fixture.cases.map(c => {
  let baseline = null;
  return c.samples.map(sample => {
    const result = step(baseline, sample, c.window);
    baseline = result.baseline;
    return result;
  });
});
process.stdout.write(JSON.stringify(output));
