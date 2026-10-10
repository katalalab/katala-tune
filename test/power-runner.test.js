// 確認ダイアログ中の排他と、cancel/例外後の解放を実際のrunnerで確認する。
'use strict';
const {test}=require('node:test');const assert=require('node:assert/strict');
const {makeRunner}=require('../lib/runner');
const node={id:'test',alias:'test',os:'windows',local:true};
const action={type:'task-run',params:{path:'\\',name:'TestTask'}};
test('Electron runner serializes same-node confirmation and releases on cancel or exception',async()=>{
 let release, confirmCount=0, mode='pending';
 const deps={loadConfig:()=>({nodes:[node],protect:[]}),confirm:()=>{confirmCount++;if(mode==='throw')throw Error('dialog failure');if(mode==='cancel')return Promise.resolve(false);return new Promise(r=>{release=r;});},db:{addAction(){},finishAction(){return true;}},exec:()=>{throw Error('must never run without approval');}};
 const runner=makeRunner(deps), first=runner.confirmAndRun('test',action,'test');
 const second=await runner.confirmAndRun('test',action,'test');assert.match(second.refused,/確認または実行中/);assert.equal(confirmCount,1);
 release(false);assert.equal((await first).cancelled,true);
 mode='throw';await assert.rejects(runner.confirmAndRun('test',action,'test'),/dialog failure/);
 mode='cancel';assert.equal((await runner.confirmAndRun('test',action,'test')).cancelled,true);
});
