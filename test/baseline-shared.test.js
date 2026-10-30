'use strict';
const test=require('node:test'),assert=require('node:assert/strict');
const input=require('../crates/tune-core/tests/fixtures/hourly-baseline.json');
const {run}=require('../crates/tune-core/tests/js/hourly_baseline');
test('shared baseline scenarios meet literal expected states, counts and interpolated evidence',()=>{
  const reports=run(input);
  for(const kind of ['pure','saved'])input[kind].forEach((s,i)=>{
    const r=reports[kind][i],e=s.expect;
    assert.equal(r.state,e.state,s.name);
    if(e.reason)assert.ok(r.reasons.includes(e.reason),s.name);
    for(const k of ['history_hours','matched_hours','recent_hours','conflicting_hours','duplicate_points','ignored_points','accepted_hours'])if(k in e)assert.equal(r.coverage[k],e[k],s.name);
    for(const k of ['raw_rows','history_rows','omitted_raw_rows'])if(k in e)assert.equal(r.source[k],e[k],s.name);
    if('slope'in e)assert.equal(r.evidence.trend.slope_pp_per_hour,e.slope,s.name);
    if('change'in e)assert.equal(r.evidence.trend.change_pp,e.change,s.name);
    if('trend_assessed'in e)assert.equal(r.evidence.trend.assessed,e.trend_assessed,s.name);
    if('elevated_hours'in e)assert.equal(r.evidence.elevated_hours,e.elevated_hours,s.name);
    if(e.profile){const [weekday,slot,p10,median,p90]=e.profile,p=r.reference.profiles.find(p=>p.weekday===weekday&&p.slot===slot);assert.deepEqual([p.p10,p.median,p.p90],[p10,median,p90],s.name);}
    if(r.state==='insufficient')assert.deepEqual(r.observations,[],s.name);
  });
});
