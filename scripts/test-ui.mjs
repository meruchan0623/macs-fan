import assert from 'node:assert/strict';
import {evaluateCurve,validTemperature,validRpm,validateSnapshot,selectSensors,sensorDetail,sensorFreshness,nextSensorExpiry} from '../ui/logic.mjs';
const curve={minRpm:1200,maxRpm:6000,lowerTemp:45,upperTemp:85};
assert.equal(evaluateCurve(curve,65),3600);assert.equal(evaluateCurve(curve,0),1200);assert.equal(evaluateCurve(curve,127),6000);
for(const c of [{...curve,upperTemp:45},{...curve,minRpm:NaN},{...curve,maxRpm:Infinity}]) assert.throws(()=>evaluateCurve(c,50));
for(const temperatureC of [-40,-10,0,127,140,150]) assert.equal(validTemperature({quality:'valid',temperatureC}),true);
for(const temperatureC of [-41,151,NaN,Infinity,null]) assert.equal(validTemperature({quality:'valid',temperatureC}),false);
assert.equal(validTemperature({quality:'invalid',temperatureC:40}),false);
assert.equal(validRpm({quality:'valid',rpm:0}),true);assert.equal(validRpm({quality:'unavailable',rpm:0}),false);
const sample={status:'ready',fans:[],sensors:[],capturedAtMs:100000};
assert.equal(validateSnapshot(sample,100001),sample);assert.throws(()=>validateSnapshot(sample,110001));assert.throws(()=>validateSnapshot(sample,94999));
console.log('UI contract / curve / invalid / stale boundaries: passed');

const mapped=[
 {id:'avg:cpu',name:'CPU 平均',classification:'derived',primary:true,quality:'valid',temperatureC:45},
 {id:'Tp01',name:'CPU 核心 1',aliases:['Performance Core 2'],classification:'mapped',primary:false,quality:'valid',temperatureC:45},
 {id:'TB0T',name:'电池',classification:'mapped',primary:true,quality:'valid',temperatureC:25},
 {id:'Tfoo',name:'Tfoo',classification:'raw',primary:false},
];
assert.deepEqual(selectSensors(mapped).map(x=>x.id),['avg:cpu','TB0T']);
assert.equal(selectSensors(mapped,'components').length,3);
assert.equal(selectSensors(mapped,'all','Tp01')[0].id,'Tp01');
assert.equal(selectSensors(mapped,'overview','Tp01').length,0);
assert.equal(selectSensors([mapped[3]],'overview').length,1);
assert.ok(sensorDetail({quality:'valid',temperatureC:2,classification:'mapped',interpretation:'低值可疑'}).includes('低值可疑'));
assert.ok(sensorDetail({quality:'unavailable',classification:'derived'}).includes('读取不可用'));
console.log('Mapped / raw / overview sensor selection: passed');

assert.deepEqual(selectSensors(mapped,'components','performance core 2').map(x=>x.id),['Tp01']);

const rawSource={id:'TZ9X',name:'TZ9X',classification:'raw',primary:false,quality:'valid',temperatureC:37};
const gpuSource={id:'gpu:registry:0000000000000001',name:'GPU',classification:'mapped',primary:true};
for(const reading of [
 {quality:'unavailable',temperatureC:null}, {quality:'invalid',temperatureC:null},
 {quality:'valid',temperatureC:0,assessment:'suspect'},
 {quality:'valid',temperatureC:40,assessment:'suspect'},
]) {
 const sensors=[rawSource,{...gpuSource,...reading}];
 assert.deepEqual(selectSensors(sensors,'overview').map(s=>s.id),sensors.map(s=>s.id));
 assert.deepEqual(selectSensors(sensors,'components').map(s=>s.id),[gpuSource.id]);
 assert.deepEqual(selectSensors(sensors,'overview','TZ9X').map(s=>s.id),[rawSource.id]);
}
assert.deepEqual(selectSensors([rawSource,{...gpuSource,quality:'valid',temperatureC:42}]).map(s=>s.id),[gpuSource.id]);
console.log('Unavailable / invalid / suspect primary keeps raw overview fallback: passed');

const diskNow=100000,disk={id:'disk:0',name:'SSD',source:'Disk',classification:'mapped',primary:true,quality:'valid',temperatureC:47,sampledAtMs:70000,expiresAtMs:115000};
assert.equal(validTemperature(disk,diskNow),true);
assert.equal(validTemperature(disk,114999),true);
assert.equal(validTemperature(disk,115000),false);
assert.equal(sensorFreshness(disk,115000),'expired');
assert.equal(nextSensorExpiry([rawSource,disk],diskNow),115000);
assert.equal(nextSensorExpiry([disk],115000),null);
assert.equal(validTemperature({...disk,sampledAtMs:0,expiresAtMs:45000},0),true);
for(const patch of [
 {sampledAtMs:undefined},{expiresAtMs:undefined},{sampledAtMs:null},{expiresAtMs:null},
 {sampledAtMs:'70000'},{expiresAtMs:'115000'},{sampledAtMs:NaN},{expiresAtMs:Infinity},
 {sampledAtMs:-1},{sampledAtMs:diskNow+1,expiresAtMs:diskNow+1000},
 {expiresAtMs:0},{expiresAtMs:-1},{expiresAtMs:70000},{expiresAtMs:69999},{expiresAtMs:115001},
 {sampledAtMs:70000.5},{expiresAtMs:115000.5},
 {sampledAtMs:Number.MAX_SAFE_INTEGER+1},{expiresAtMs:Number.MAX_SAFE_INTEGER+1},
 {sampledAtMs:0,expiresAtMs:Number.MAX_SAFE_INTEGER},
]) {
 const invalid={...disk,...patch};
 assert.equal(validTemperature(invalid,diskNow),false,JSON.stringify(patch));
 assert.equal(sensorFreshness(invalid,diskNow),'invalid');
 assert.equal(nextSensorExpiry([invalid],diskNow),null);
}
for(const partial of [{sampledAtMs:70000},{expiresAtMs:115000}])assert.equal(validTemperature({quality:'valid',temperatureC:47,...partial},diskNow),false);
for(const patch of [{quality:'invalid'},{quality:'unavailable',temperatureC:null},{temperatureC:151}])assert.equal(validTemperature({...disk,...patch},diskNow),false);
assert.equal(validTemperature(rawSource,Number.MAX_SAFE_INTEGER),true); // Legacy sources retain snapshot-level freshness.
assert.deepEqual(selectSensors([rawSource,disk],'overview','',114999).map(s=>s.id),[disk.id]);
assert.deepEqual(selectSensors([rawSource,disk],'overview','',115000).map(s=>s.id),[rawSource.id,disk.id]);
const diskDetail=sensorDetail(disk,diskNow);
assert.ok(diskDetail.includes('采样于'));
assert.ok(!diskDetail.includes('本次已解码'));
assert.ok(sensorDetail(disk,115000).includes('读数已过期'));
assert.ok(sensorDetail({...disk,sampledAtMs:null},diskNow).includes('采样时刻无效'));
const cachedDisk=structuredClone(disk);
validateSnapshot({...sample,capturedAtMs:114999,sensors:[cachedDisk]},114999);
assert.equal(cachedDisk.sampledAtMs,70000);
assert.equal(cachedDisk.expiresAtMs,115000);
assert.equal(validTemperature(cachedDisk,115000),false); // A new snapshot cannot renew the source.
console.log('Per-source timestamps / strict future rejection / partial and malformed timing / expiry / cached age / overview fallback: passed');


const {PreferenceSession,copy,defaultView,emptyDocument,draftChanged,createPreset,newMode,validatePreset,presetWarnings,temperatureText}=await import('../ui/preferences-logic.mjs');
const machine={model:'Mac16,8',architecture:'aarch64',fans:[{id:'F0',minRpm:1200,maxRpm:6000}],sensors:[{id:'Tp01',quality:'valid'}]};
const fresh=createPreset(machine);fresh.name=' 安静 ';
assert.deepEqual(fresh.fans,[{fanId:'F0',mode:{type:'automatic'}}]);
assert.equal(validatePreset(fresh).name,'安静');
assert.throws(()=>createPreset({...machine,fans:[]}));
for(const name of ['', ' \n ', 'a\u0000b', '长'.repeat(65)])assert.throws(()=>validatePreset({...fresh,name}));
assert.throws(()=>validatePreset(fresh,[{...fresh,id:'existing',name:'安静'}]));
for(const mode of [{type:'fixed',rpm:NaN},{type:'fixed',rpm:12.3},{type:'fixed',rpm:30001},{type:'sensor',sensorId:'',...curve},{type:'sensor',sensorId:'Tp01',...curve,upperTemp:128},{type:'unknown'}])assert.throws(()=>validatePreset({...fresh,fans:[{fanId:'F0',mode}]}));
assert.deepEqual(newMode('fixed',machine.fans[0]),{type:'fixed',rpm:1200});
const missing={...fresh,model:'Other1,1',fans:[{fanId:'F1',mode:{type:'sensor',sensorId:'missing',...curve}}]};
assert.equal(validatePreset(missing).fans[0].mode.sensorId,'missing'); // Portable references remain intact.
assert.equal(presetWarnings(missing,machine).length,3);
assert.equal(presetWarnings({...fresh,fans:[{fanId:'F0',mode:{type:'fixed',rpm:100}}]},machine).length,1);
const raw=copy(machine);assert.equal(temperatureText(0,{unit:'fahrenheit',precision:'whole'}),'32 °F');assert.equal(temperatureText(37,{unit:'fahrenheit',precision:'decimal'}),'98.6 °F');assert.equal(temperatureText(37,defaultView()),'37.0 °C');assert.equal(temperatureText(NaN),'—');assert.deepEqual(raw,machine);
const baseline=copy(fresh),draft=copy(fresh);draft.name='工作';assert.equal(draftChanged(draft,baseline),true);assert.equal(baseline.name,' 安静 ');assert.equal(draftChanged(copy(baseline),baseline),false);
const calls=[];let stored=emptyDocument(),rejectNext=false;
const session=new PreferenceSession(async(command,args)=>{
 calls.push({command,args:copy(args??null)});
 if(command==='preferences_read')return copy(stored);
 assert.equal(args.expectedRevision,stored.revision);
 if(rejectNext){rejectNext=false;throw new Error('配置已被其他窗口或进程修改');}
 stored={...stored,revision:stored.revision+1};
 if(args.update.type==='setView')stored.view=copy(args.update.view);
 if(args.update.type==='savePreset'){const p={...args.update.preset,id:'a'.repeat(32)};stored.presets=[p];stored.editorPresetId=p.id;}
 return copy(stored);
});
await assert.rejects(session.update({type:'setView',view:defaultView()}),/尚未读取/);
await session.read();
const update={type:'savePreset',preset:validatePreset(draft)};
const saved=session.update(update);update.preset.name='caller mutation';
const changedView=session.update({type:'setView',view:{...defaultView(),unit:'fahrenheit'}});
await Promise.all([saved,changedView]);
assert.equal(session.document.presets[0].name,'工作');assert.equal(session.document.revision,2);assert.equal(draft.name,'工作');assert.equal(draft.id,null);
assert.deepEqual(calls.filter(c=>c.command==='preferences_update').map(c=>c.args.expectedRevision),[0,1]);
rejectNext=true;const committed=copy(session.document);
await assert.rejects(session.update({type:'savePreset',preset:validatePreset(draft)}),/其他窗口/);
assert.deepEqual(session.document,committed);assert.equal(draftChanged(draft,baseline),true);assert.equal(draft.name,'工作');
await session.update({type:'setView',view:defaultView()});assert.equal(session.document.revision,3); // Queue recovers after failure.
stored={...stored,revision:8};await session.read();assert.equal(session.document.revision,8);assert.equal(draft.name,'工作');
const offline=new PreferenceSession();await assert.rejects(offline.read(),/浏览器无法保存/);assert.equal(offline.loaded,false);
console.log('Preset validation / portable references / units / drafts / serialized revisions / conflict recovery / no-IPC: passed');

const {createDiagnosticQuery}=await import('../ui/diagnostics.mjs');
const diagnosticCalls=[];let releaseInspection;
const diagnosticQuery=createDiagnosticQuery(async command=>{
 diagnosticCalls.push(command);
 if(command==='service_status')throw new Error('status unavailable');
 return await new Promise(resolve=>{releaseInspection=resolve;});
});
const firstDiagnostic=diagnosticQuery(),joinedDiagnostic=diagnosticQuery();
assert.equal(firstDiagnostic,joinedDiagnostic);
await new Promise(resolve=>setImmediate(resolve));
assert.deepEqual(diagnosticCalls,['service_status','helper_inspect']);
const blockedHelper={result:'blocked',reason:'production_team_unconfigured',authenticated_response:false,hardware_write_enabled:false};
releaseInspection(blockedHelper);
const diagnosticResult=await firstDiagnostic;
assert.equal(diagnosticResult.service.result,'unavailable');
assert.equal(diagnosticResult.helper,blockedHelper);
assert.ok(diagnosticResult.helperCompletedAtMs<=Date.now());
const retryDiagnostic=diagnosticQuery();
assert.notEqual(firstDiagnostic,retryDiagnostic);
await new Promise(resolve=>setImmediate(resolve));
releaseInspection(blockedHelper);await retryDiagnostic;
assert.deepEqual(diagnosticCalls,['service_status','helper_inspect','service_status','helper_inspect']);
const hungCalls=[],updates=[];let releaseStatus;
const hungQuery=createDiagnosticQuery(command=>{
 hungCalls.push(command);
 return command==='service_status'?new Promise(resolve=>{releaseStatus=resolve;}):Promise.resolve(blockedHelper);
},update=>updates.push(update),20);
const hungResult=await hungQuery();
assert.ok(updates.some(update=>update.helper===blockedHelper&&update.service===null));
assert.equal(hungResult.service.reason,'diagnostic_query_timeout');
const timeoutAt=hungResult.serviceCompletedAtMs;
await hungQuery(); // A timed-out native request is still in flight.
assert.equal(hungCalls.filter(x=>x==='service_status').length,1);
assert.equal(updates.at(-1).serviceCompletedAtMs,timeoutAt);
const beforeLateReply=updates.length;
releaseStatus({result:'observed'});
await new Promise(resolve=>setImmediate(resolve));
assert.equal(updates.length,beforeLateReply); // Late success cannot replace timeout.
const freshQuery=hungQuery();
await new Promise(resolve=>setImmediate(resolve));
releaseStatus({result:'fresh'});
assert.equal((await freshQuery).service.result,'fresh');
assert.equal(hungCalls.filter(x=>x==='service_status').length,2);
console.log('Diagnostics single-flight / independent publication / timeout / bounded native jobs / late reply rejection / retry: passed');


// Execute the actual app with a small DOM and controlled clock; no browser or visual claim.
const {readFile}=await import('node:fs/promises');
const {runInNewContext}=await import('node:vm');
const logic=await import('../ui/logic.mjs');
class TestElement {
 constructor(){this.children=[];this.value='';this.textContent='';this.listeners={};this.replacements=0;this.classList={toggle(){}};}
 append(...children){this.children.push(...children);}
 replaceChildren(...children){this.children=children;this.replacements++;}
 setAttribute(name,value){this[name]=value;}
 addEventListener(name,fn){this.listeners[name]=fn;}
}
const elements=new Map();
const getElement=id=>{if(!elements.has(id))elements.set(id,new TestElement());return elements.get(id);};
getElement('sensor-scope').value='overview';getElement('sensor-sort').value='temperature';
let appNow=100000,appSnapshot={...sample,capturedAtMs:appNow,sensors:[rawSource,{...disk,sampledAtMs:57000,expiresAtMs:102000}]},blockSnapshot=false;
const intervals=[];let timerId=0;
const appSource=(await readFile(new URL('../ui/app.js',import.meta.url),'utf8')).replace(/^import .*;\n/gm,'');
runInNewContext(appSource,{
 ...logic,validateSnapshot:value=>validateSnapshot(value,appNow),temperatureText,defaultView,URLSearchParams,location:{search:''},
 Date:class extends Date {constructor(...args){super(...(args.length?args:[appNow]));}static now(){return appNow;}},
 document:{getElementById:getElement,createElement:()=>new TestElement(),querySelectorAll:()=>[]},
 window:{__TAURI__:{core:{invoke:command=>command==='snapshot'?(blockSnapshot?new Promise(()=>{}):Promise.resolve(structuredClone(appSnapshot))):Promise.resolve({})}},addEventListener(){}},
 createControl:()=>({status(){},dispose(){}}),createDiagnosticQuery:()=>()=>{},setupPreferences:()=>({snapshotChanged(){},controlChanged(){}}),
 setTimeout:()=>++timerId,clearTimeout(){},setInterval:callback=>{intervals.push(callback);},navigator:{},
});
await new Promise(resolve=>setImmediate(resolve));
assert.equal(getElement('error-detail').textContent,'');
assert.equal(getElement('sensor-rows').children.length,1);
assert.equal(getElement('sensor-rows').children[0].children[1].textContent,'47.0');
appNow=56000;intervals[0](); // Clock rollback makes the already-rendered sample future-dated.
assert.equal(getElement('sensor-rows').children.length,2);
assert.equal(getElement('sensor-rows').children[1].children[1].textContent,'—');
assert.ok(getElement('sensor-rows').children[1].title.includes('采样时刻无效'));
const renderedAfterRollback=getElement('sensor-rows').replacements;
intervals[0]();assert.equal(getElement('sensor-rows').replacements,renderedAfterRollback);
appNow=100000;intervals[0](); // Time becomes valid again without requiring a new snapshot.
assert.equal(getElement('sensor-rows').children.length,1);
assert.equal(getElement('sensor-rows').children[0].children[1].textContent,'47.0');
appNow=101000;appSnapshot={...appSnapshot,capturedAtMs:appNow};
getElement('refresh').listeners.click();await new Promise(resolve=>setImmediate(resolve));
assert.equal(getElement('sensor-rows').children.length,1);
blockSnapshot=true;getElement('refresh').listeners.click(); // Poll stays blocked while the source expires.
appNow=102000;intervals[0]();
const expiredRows=getElement('sensor-rows');
assert.equal(expiredRows.children.length,2); // Raw overview fallback returns.
assert.equal(expiredRows.children[0].children[0].textContent,rawSource.name); // Expired reading no longer sorts first.
assert.equal(expiredRows.children[1].children[1].textContent,'—');
assert.ok(expiredRows.children[1].title.includes('读数已过期'));
const renderedAtExpiry=expiredRows.replacements;
appNow=103000;intervals[0]();
assert.equal(expiredRows.replacements,renderedAtExpiry); // No repeated table rebuild after crossing.
console.log('Actual app controlled DOM / clock rollback and recovery / cached snapshot / blocked poll / expiry repaint / temperature sorting / one-time rebuild: passed');
