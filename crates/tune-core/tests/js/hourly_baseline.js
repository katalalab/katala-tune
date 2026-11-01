'use strict';
const {analyzeHours} = require('../../../../lib/hourly-baseline');
const {readBaseline} = require('../../../../lib/saved-baseline');
const retention = require('../../../../lib/metrics-retention');
const {DatabaseSync} = require('node:sqlite');
const H=3600000,D=24*H,M=60000;
function points(s,end) {
  let qi=0;
  let xs=Array.from({length:(s.days??28)*24},(_,i)=>{
    const at=end-(s.days??28)*D+i*H,d=new Date(at);
    let mean=s.base??20;
    if(s.pattern==='weekly') mean=([0,6].includes(d.getUTCDay())?10:d.getUTCHours()>=8&&d.getUTCHours()<16?80:20)+d.getUTCHours()%4;
    if(s.quantiles && at<end-D && d.getUTCDay()===1 && d.getUTCHours()>=8 && d.getUTCHours()<12) mean=qi++;
    if(at>=end-D && s.recent) mean=s.recent[(at-end+D)/H];
    return {at,mean,minutes:at>=end-D?(s.recent_minutes??60):60,resolution:s.resolution??'hour'};
  }).filter(p=>p.at<end-D || !(s.drop_recent??[]).includes((p.at-end+D)/H));
  if(s.duplicate) xs.push({...xs.at(-1)});
  if(s.conflict) xs.push({...xs.at(-1),mean:99});
  if(s.future) for(let i=0;i<2;i++)xs.push({at:end+i*H,mean:100,minutes:60,resolution:'hour'});
  if(s.repeat) xs=Array(s.repeat).fill(xs[0]);
  return s.reverse?xs.reverse():xs;
}
function saved(s,end) {
  const db=new DatabaseSync(':memory:');
  try {
    db.exec('CREATE TABLE metrics_minute(node_id TEXT,minute INTEGER,n INTEGER,v TEXT,src TEXT,PRIMARY KEY(node_id,minute)) WITHOUT ROWID;');
    retention.initialize(db);
    db.exec('BEGIN');
    for(const o of s.operations) {
      if(o.type==='state')db.prepare('UPDATE metrics_retention_state SET sealed_before=?,last_now=?').run(o.seal,o.last);
      else if(o.type==='missing_state')db.exec('DELETE FROM metrics_retention_state');
      else if(o.type==='hour')db.prepare('INSERT INTO metrics_history VALUES (?,?,?,?)').run(o.node??'fixture',o.resolution??'hour',o.at,JSON.stringify({mem:o.value}));
      else if(o.type==='archive_payload')db.prepare("INSERT INTO metrics_history VALUES ('fixture','hour',?,?)").run(o.at,o.text);
      else if(o.type==='payload') {
        for(let i=0;i<o.count;i++)db.prepare("INSERT INTO metrics_minute VALUES ('fixture',?,60,?,NULL)").run(o.at+i*M,o.text);
      } else if(o.type==='grid') {
        for(let h=0;h<o.hours;h++) {
          const at=o.at+h*H;
          if(o.archived)db.prepare("INSERT INTO metrics_history VALUES ('fixture','hour',?,?)").run(at,JSON.stringify({mem:[o.value*30,30,o.value]}));
          else for(let m=0;m<30;m++)db.prepare("INSERT INTO metrics_minute VALUES ('fixture',?,60,?,NULL)").run(at+m*M,JSON.stringify({mem:[o.value,o.value]}));
        }
      } else if(o.type==='minutes') {
        const put=db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?,?,?,?,NULL)');
        for(let i=0;i<o.count;i++) {
          const v=JSON.stringify({mem:o.value});
          put.run(o.node??'fixture',o.at+i*(o.step??M),typeof o.n==='string'?BigInt(o.n):o.n??60,o.blob?Buffer.from(v):v);
        }
      } else throw Error('invalid test fixture operation');
    }
    db.exec('COMMIT; PRAGMA query_only=ON');
    return readBaseline(db,'fixture','mem',end+H/2);
  } finally {db.close();}
}
function run(input) {return {pure:input.pure.map(s=>analyzeHours(points(s,input.end),{metric:s.metric??'mem',now:input.end+H/2})),saved:input.saved.map(s=>saved(s,input.end))};}
if(require.main===module) {
  const fs=require('node:fs');
  const input=JSON.parse(fs.readFileSync(process.argv[2]??0,'utf8'));
  process.stdout.write(JSON.stringify(run(input)));
}
module.exports={points,saved,run};
