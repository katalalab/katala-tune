'use strict';
const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const source=fs.readFileSync(require.resolve('../renderer/app.js'),'utf8');
const binding=source.slice(source.indexOf('function bindSearch('),source.indexOf('\nfunction page('));
test('search saves the draft immediately and detached debounce cannot restore a cleared query', async()=>{
 let q='',renders=0,next=0;const timers=new Map();
 const old={isConnected:true,value:'typed',focus(){},setSelectionRange(){}};let input=old;
 const ctx=vm.createContext({state:{view:'overview'},$:()=>input,setTimeout:fn=>{timers.set(++next,fn);return next;},clearTimeout:id=>timers.delete(id),apply:v=>{q=v;},render:()=>{renders++;}});
 vm.runInContext(binding+"\nbindSearch('#q','overview',apply,render);",ctx);
 old.oninput({target:old});assert.equal(q,'typed');
 old.isConnected=false;input={...old,isConnected:true,value:''};q='';
 for(const fn of timers.values())await fn();
 assert.equal(q,'');assert.equal(renders,0);
});
test('canceling a pending search prevents a later redraw', async()=>{
 let renders=0,next=0;const timers=new Map();const input={isConnected:true,value:'typed',focus(){},setSelectionRange(){}};
 const ctx=vm.createContext({state:{view:'overview'},$:()=>input,setTimeout:fn=>{timers.set(++next,fn);return next;},clearTimeout:id=>timers.delete(id),apply(){},render:()=>{renders++;}});
 vm.runInContext(binding+"\ncancel=bindSearch('#q','overview',apply,render);",ctx);input.oninput({target:input});ctx.cancel();
 for(const fn of timers.values())await fn();assert.equal(renders,0);
});

test('normal debounced redraw preserves a caret or selection inside the query', async()=>{
 let next=0;const timers=new Map();let selection;
 const old={isConnected:true,value:'search-query',selectionStart:2,selectionEnd:5,selectionDirection:'backward'};
 let input=old;
 const ctx=vm.createContext({state:{view:'overview'},$:()=>input,setTimeout:fn=>{timers.set(++next,fn);return next;},clearTimeout:id=>timers.delete(id),apply(){},render(){old.isConnected=false;input={value:old.value,focus(){},setSelectionRange(...args){selection=args;}};}});
 vm.runInContext(binding+"\nbindSearch('#q','overview',apply,render);",ctx);old.oninput({target:old});
 for(const fn of timers.values())await fn();
 assert.deepEqual(selection,[2,5,'backward']);
});
