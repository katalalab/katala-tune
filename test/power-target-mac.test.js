// 本番のshellを実行し、pmsetだけ偽物へ差し替える。実機の電源設定には触れない。
'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs=require('node:fs'), os=require('node:os'), path=require('node:path');
const { spawnSync }=require('node:child_process');
const { plan }=require('../lib/actions');
function check(key,mode) {
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'tune-power-action-test-'));
 try {
  const state=path.join(dir,'state');fs.writeFileSync(state,'0');
  const script=plan({id:'test-mac',os:'macos'},{type:'set-low-power-mode',params:{source:'ac',enabled:true,prev:false}},{protect:[]}).script;
  const body=script.replaceAll('/tmp/org.katala.tune-power-control.lock','$KATALA_TEST_DIR/power-action.lock');
  const setup=`pmset() {
 if [ "$1" = -g ]; then
  [ "$KATALA_TEST_MODE" = readfail ] && [ -f "$KATALA_TEST_DIR/writes" ] && return 1
  printf 'AC Power:\n %s %s\nBattery Power:\n %s 0\n' "$KATALA_TEST_KEY" "$(cat "$KATALA_TEST_DIR/state")" "$KATALA_TEST_KEY"; return 0
 fi
 [ "$2" = "$KATALA_TEST_KEY" ] || return 4
 printf '%s\n' "$3" >> "$KATALA_TEST_DIR/writes"
 printf '%s' "$3" > "$KATALA_TEST_DIR/state"
 if [ "$KATALA_TEST_MODE" = third ]; then printf 2 > "$KATALA_TEST_DIR/state"; fi
 if [ "$KATALA_TEST_MODE" = applyfail ] && [ "$(wc -l < "$KATALA_TEST_DIR/writes")" -eq 1 ]; then return 1; fi
 return 0
}
${body}`;
  const r=spawnSync('/bin/bash',['-c',setup],{env:{...process.env,KATALA_TEST_DIR:dir,KATALA_TEST_KEY:key,KATALA_TEST_MODE:mode},encoding:'utf8',timeout:10000});
  assert.equal(fs.existsSync(path.join(dir,'power-action.lock')),false,r.stderr);
  const writes=fs.existsSync(path.join(dir,'writes'))?fs.readFileSync(path.join(dir,'writes'),'utf8').trim().split('\n'):[];
  return {code:r.status,state:fs.readFileSync(state,'utf8'),writes,out:r.stdout,err:r.stderr};
 }finally { fs.rmSync(dir,{recursive:true,force:true}); }
}
for(const key of ['powermode','lowpowermode']) test(`mac ${key}: native shell success, failure restore, third state and missing readback`,{skip:process.platform==='win32'},()=>{
 const a=check(key,'success');assert.equal(a.code,0,a.err);assert.equal(a.state,'1');assert.deepEqual(a.writes,['1']);
 const b=check(key,'applyfail');assert.equal(b.code,6,b.err);assert.equal(b.state,'0');assert.deepEqual(b.writes,['1','0']);
 const c=check(key,'third');assert.equal(c.code,7,c.err);assert.equal(c.state,'2');assert.deepEqual(c.writes,['1']);
 const d=check(key,'readfail');assert.equal(d.code,7,d.err);assert.deepEqual(d.writes,['1']);
});
