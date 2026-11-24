import assert from 'node:assert/strict';
import {createControl} from '../ui/control.mjs';
import {PreferenceSession,emptyDocument} from '../ui/preferences-logic.mjs';
const drain=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const reply=(operation,state='idle',extras={})=>({schema_version:1,operation,state,reason:'ok',hardware_write_enabled:true,authenticated_response:true,current:true,last_response_age_ms:0,plan:null,authorization_live:false,request_in_flight:false,can_attempt_start:true,can_stop:false,...extras});
function harness(options={}){
 const calls=[];let serial=0;
 const control=createControl({pollMs:100000,timeoutMs:1000,newRequestId:()=>`request-${++serial}`,invoke:(command,args)=>new Promise((resolve,reject)=>calls.push({command,args,resolve,reject})),...options});
 return {control,calls,async ready(){const pending=control.status();await drain();calls.at(-1).resolve(reply('control_status'));await pending;await drain();}};
}
{
 const {control,calls,ready}=harness();await ready();
 const first=control.start({presetId:'saved',expectedRevision:7});await drain();
 assert.deepEqual(calls.at(-1).args,{presetId:'saved',expectedRevision:7,requestId:'request-1'});
 const start=calls.at(-1);assert.equal(control.view().label,'未确认');assert.equal(control.view().canStop,true);
 assert.equal(control.start({presetId:'another',expectedRevision:8}),first);
 const stop=control.stop();await drain();assert.equal(calls.at(-1).command,'control_stop');assert.deepEqual(calls.at(-1).args,{cancelRequestId:'request-1'});
 calls.at(-1).resolve(reply('stop_plan','automatic_confirmed'));await stop;await drain();
 start.resolve(reply('start_plan','active',{authorization_live:true,can_stop:true}));await first;await drain();
 assert.equal(control.view().label,'只读');assert.equal(control.view().diagnostic.response.operation,'stop_plan');
 const stopAgain=control.stop();await drain();assert.deepEqual(calls.at(-1).args,{cancelRequestId:null});calls.at(-1).resolve(reply('stop_plan'));await stopAgain;control.dispose();
}
{
 const {control,calls,ready}=harness();await ready();
 const oldStatus=control.status();await drain();const old=calls.at(-1);
 const start=control.start({presetId:'saved',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','active',{authorization_live:true,can_stop:true}));await start;await drain();
 old.resolve(reply('control_status','idle'));await oldStatus;await drain();assert.equal(control.view().label,'控制中');control.dispose();
}
{
 const {control,calls,ready}=harness({timeoutMs:10});await ready();
 const start=control.start({presetId:'saved',expectedRevision:1});await drain();const pending=calls.at(-1);
 await start;assert.equal(control.view().message,'响应超时');assert.equal(control.view().diagnostic.inFlight.start,true);
 const count=calls.length;await control.start({presetId:'saved',expectedRevision:1});await drain();assert.equal(calls.length,count);assert.equal(control.view().canStop,true);
 const stop=control.stop();await drain();calls.at(-1).resolve(reply('stop_plan'));await stop;
 pending.resolve(reply('start_plan','active',{authorization_live:true,can_stop:true}));await drain();assert.equal(control.view().label,'只读');control.dispose();
}
{
 for(const rejected of [false,true]){
  const {control,calls,ready}=harness({timeoutMs:10});await ready();
  const start=control.start({presetId:'saved',expectedRevision:1});await drain();const pending=calls.at(-1);
  await start;assert.equal(control.view().message,'响应超时');
  if(rejected)pending.reject(new Error('late rejection'));else pending.resolve(reply('start_plan','active',{authorization_live:true,can_stop:true}));
  await drain();assert.equal(control.view().label,'未确认');assert.equal(control.view().diagnostic.uncertain,true);assert.equal(control.view().diagnostic.inFlight.start,false);assert.equal(control.view().diagnostic.lastFailure,null);
  assert.equal(calls.at(-1).command,'control_status');assert.equal(control.view().diagnostic.response.operation,'control_status');
  calls.at(-1).resolve(reply('control_status','active',{authorization_live:true,can_stop:true}));await drain();assert.equal(control.view().label,'控制中');control.dispose();
 }
}
{
 let time=0;const {control,calls}=harness({now:()=>time});
 let pending=control.status();await drain();calls.at(-1).resolve(reply('control_status','recovery_pending',{can_stop:true}));await pending;await drain();assert.equal(control.view().label,'恢复中');
 time=1501;assert.equal(control.view().label,'未确认');assert.equal(control.view().canStop,true);
 pending=control.status();await drain();calls.at(-1).resolve(reply('control_status','recovery_pending',{current:false,can_stop:false}));await pending;await drain();assert.equal(control.view().label,'未确认');assert.equal(control.view().canStop,true);control.dispose();
}
{
 let time=0;const {control,calls,ready}=harness({now:()=>time});await ready();
 const start=control.start({presetId:'saved',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','active',{authorization_live:true,can_stop:true,last_response_age_ms:300}));await start;await drain();
 assert.equal(control.view().active,true);time=1201;assert.equal(control.view().label,'未确认');assert.equal(control.view().active,false);assert.equal(control.view().canStop,true);
 const status=control.status();await drain();calls.at(-1).resolve(reply('control_status','active',{authorization_live:false}));await status;assert.equal(control.view().label,'未确认');control.dispose();
}
{
 for(const evidence of [{authenticated_response:false},{hardware_write_enabled:false},{current:false},{authorization_live:false}]){
  const {control,calls,ready}=harness();await ready();const start=control.start({presetId:'saved',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','active',{authorization_live:true,...evidence}));await start;assert.equal(control.view().active,false);assert.equal(control.view().label,'未确认');control.dispose();
 }
 const {control,calls,ready}=harness();await ready();const start=control.start({presetId:'saved',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','active',{plan:{phase:'unobserved',fans:[{mode:'automatic'}]},authorization_live:false}));await start;assert.equal(control.view().automatic,false);assert.equal(control.view().label,'未确认');control.dispose();
}
{
 const {control,calls}=harness();const status=control.status();await drain();calls.at(-1).resolve(reply('control_status','blocked',{reason:'production_team_unconfigured',hardware_write_enabled:false,can_attempt_start:false,authenticated_response:false,current:false,last_response_age_ms:null}));await status;await drain();
 assert.equal(control.view().canAttemptStart,false);assert.equal(control.view().label,'只读');assert.equal(control.view().message,'控制服务未配置');await control.start({presetId:'saved',expectedRevision:1});assert.equal(calls.length,1);control.dispose();
}
{
 const {control,calls,ready}=harness();await ready();const start=control.start({presetId:'saved',expectedRevision:1});await drain();calls.at(-1).reject(new Error('authorization denied'));await start;await drain();assert.equal(control.view().label,'未确认');assert.equal(control.view().active,false);assert.equal(control.view().canStop,true);assert.match(control.view().diagnostic.lastFailure,/authorization denied/);control.dispose();
}
{
 const {control,calls}=harness();const first=control.status();await drain();assert.equal(control.status(),first);assert.equal(calls.length,1);calls[0].resolve({schema_version:1,state:'active'});await first;assert.equal(control.view().active,false);assert.equal(control.view().message,'控制响应无效');control.dispose();
}
{
 const control=createControl();await control.status();await control.start({presetId:'saved',expectedRevision:1});await control.stop();assert.equal(control.view().canStop,false);assert.equal(control.view().label,'只读');control.dispose();
 const calls=[];const session=new PreferenceSession(async(command,args)=>{calls.push(command);return command==='preferences_read'?emptyDocument():{...emptyDocument(),revision:1,presets:[args.update.preset]};});
 await session.read();await session.update({type:'savePreset',preset:{id:'saved',name:'Quiet',fans:[{fanId:'F0',mode:{type:'automatic'}}]}});assert.deepEqual(calls,['preferences_read','preferences_update']);
}
{
 const plan={phase:'preparing',authorized:true,fans:[{fan:0,state:'unobserved'}]};
 for(const extra of [{},{hardware_write_enabled:false},{current:false},{authenticated_response:false},{authorization_live:false},{plan:{...plan,authorized:false}},{plan:{...plan,phase:'running'}},{plan:{...plan,fans:[{fan:0,state:'active'}]}}]){
  let time=0;const {control,calls,ready}=harness({now:()=>time});await ready();
  const start=control.start({presetId:'sensor',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','preparing',{authorization_live:true,can_stop:true,plan,...extra}));await start;await drain();
  const valid=Object.keys(extra).length===0;
  assert.equal(control.view().preparing,valid);assert.equal(control.view().active,false);assert.equal(control.view().label,valid?'准备中':'未确认');assert.equal(control.view().canStop,true);
  time=1501;assert.equal(control.view().preparing,false);assert.equal(control.view().label,'未确认');assert.equal(control.view().canStop,true);
  const stop=control.stop();await drain();calls.at(-1).resolve(reply('stop_plan','automatic_confirmed'));await stop;control.dispose();
 }
 const {control,calls,ready}=harness();await ready();let pending=control.start({presetId:'sensor',expectedRevision:1});await drain();calls.at(-1).resolve(reply('start_plan','preparing',{authorization_live:true,can_stop:true,plan}));await pending;await drain();assert.equal(control.view().label,'准备中');
 pending=control.status();await drain();calls.at(-1).resolve(reply('control_status','active',{authorization_live:true,can_stop:true,plan:{phase:'running',authorized:true}}));await pending;assert.equal(control.view().active,true);assert.equal(control.view().preparing,false);control.dispose();
}
{
 let time=0;const {control,calls,ready}=harness({now:()=>time});await ready();
 const stop=control.stop();await drain();calls.at(-1).resolve(reply('stop_plan','automatic_confirmed',{hardware_write_enabled:false,authorization_live:false,can_stop:false}));await stop;
 assert.equal(control.view().automatic,true);assert.equal(control.view().active,false);assert.equal(control.view().label,'只读');
 time=1501;assert.equal(control.view().automatic,false);assert.equal(control.view().label,'未确认');control.dispose();
 for(const extra of [{authenticated_response:false},{current:false},{last_response_age_ms:1501}]){
  const {control,calls}=harness();const status=control.status();await drain();calls.at(-1).resolve(reply('control_status','automatic_confirmed',{hardware_write_enabled:false,...extra}));await status;
  assert.equal(control.view().automatic,false);assert.equal(control.view().label,'未确认');control.dispose();
 }
}
{
 for(const state of ['active','automatic_confirmed']){
  let time=10000;const {control,calls}=harness({now:()=>time});
  let pending=control.status();await drain();calls.at(-1).resolve(reply('control_status',state,{authorization_live:true,can_stop:true}));await pending;await drain();
  assert.equal(control.view()[state==='active'?'active':'automatic'],true);
  time=10100;control.view();time=10050;
  assert.equal(control.view().label,'未确认');assert.equal(control.view().canAttemptStart,false);assert.equal(control.view().canStop,true);
  time=10100;assert.equal(control.view().label,'未确认','clock recovery cannot revive the old confirmation');
  pending=control.status();await drain();calls.at(-1).resolve(reply('control_status',state,{authorization_live:true,can_stop:true}));await pending;await drain();
  assert.equal(control.view()[state==='active'?'active':'automatic'],true,'new authenticated receipt restores a clock baseline');
  time=NaN;assert.equal(control.view().label,'未确认');assert.equal(control.view().canAttemptStart,false);control.dispose();
 }
}
console.log('Control UI: arbitration, preparing, no-write active denial, confirmed stop after disconnect, monotonic receipt clock, rollback latch, expiry and saved-only intent passed.');
