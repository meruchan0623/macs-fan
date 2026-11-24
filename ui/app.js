import {createControl} from './control.mjs';
import {createDiagnosticQuery} from './diagnostics.mjs';
import {temperatureText,defaultView} from './preferences-logic.mjs';
import {setupPreferences} from './preferences.js';
import {validTemperature,validRpm,validateSnapshot,selectSensors,sensorDetail,sensorFreshness} from './logic.mjs';
const $=id=>document.getElementById(id);
const demo=new URLSearchParams(location.search).get('demo')==='1';
const state={snapshot:null,error:null,lastModel:'本机',view:defaultView(),preferences:null,service:null,helper:null,serviceCompletedAtMs:null,helperCompletedAtMs:null,control:null};
const text=(id,value)=>{$(id).textContent=value;};
const number=n=>Number.isFinite(n)?Math.round(n).toLocaleString('zh-CN'):'—';
const element=(tag,value,cls)=>{const el=document.createElement(tag);if(value!==undefined)el.textContent=value;if(cls)el.className=cls;return el;};
const modeName=mode=>({automatic:'自动',manual:'手动'}[mode]??'未知');
function showDialog(id){const d=$(id);if(!d.open)d.showModal();}
for(const button of document.querySelectorAll('[data-close]'))button.addEventListener('click',()=>{if(button.closest('dialog').id!=='curve-dialog')button.closest('dialog').close();});

async function openDiagnostics(){
 showDialog('diagnostics-dialog');
 if(demo||typeof invoke!=='function')return;
 await queryDiagnostics();
}
function renderDiagnostics(){text('json-output',JSON.stringify({mode:demo?'demo':'monitor',error:state.error,helper:state.helper,helperCompletedAtMs:state.helperCompletedAtMs,service:state.service,serviceCompletedAtMs:state.serviceCompletedAtMs,control:state.control?.diagnostic??null,snapshot:state.snapshot},null,2));}
$('open-diagnostics').addEventListener('click',openDiagnostics);
$('status-detail').addEventListener('click',openDiagnostics);
$('sensor-info').addEventListener('click',()=>showDialog('info-dialog'));
function emptyRow(host,colspan,message){const row=element('tr'),cell=element('td',message,'empty');cell.colSpan=colspan;row.append(cell);host.append(row);}
let renderedSourceFreshness='';
const sourceFreshnessKey=(sensors,now)=>sensors.filter(s=>'sampledAtMs' in s||'expiresAtMs' in s).map(s=>sensorFreshness(s,now)).join(',');
function renderSensors(){
 const now=Date.now();
 const s=state.snapshot,all=s&&s.status!=='unavailable'?s.sensors:[];
 const query=$('sensor-search').value.trim().toLocaleLowerCase();
 const scope=$('sensor-scope').value;
 const sensors=selectSensors(all,scope,query,now);
 renderedSourceFreshness=sourceFreshnessKey(all,now);
 const sort=$('sensor-sort').value;
 sensors.sort((a,b)=>sort==='temperature'?(validTemperature(b,now)?b.temperatureC:-Infinity)-(validTemperature(a,now)?a.temperatureC:-Infinity)||String(a.id).localeCompare(String(b.id)):(({CPU:0,GPU:1,Battery:2,Other:3}[a.group]??4)-({CPU:0,GPU:1,Battery:2,Other:3}[b.group]??4)||String(a.name??a.id).localeCompare(String(b.name??b.id))));
 const rows=$('sensor-rows');rows.replaceChildren();
 for(const sensor of sensors){
  const row=element('tr'),valid=validTemperature(sensor,now),name=element('td',sensor.name??sensor.id),reading=element('td',valid?temperatureText(sensor.temperatureC,state.view,false):'—');
  const detail=sensorDetail(sensor,now);
  row.title=`${sensor.name??sensor.id} · ${detail}`;reading.setAttribute('aria-label',`${sensor.name??sensor.id}：${valid?temperatureText(sensor.temperatureC,state.view):'不可用'}，${detail}`);
  if(!valid)row.className='unavailable';if(valid&&(sensor.temperatureC===0||sensor.assessment==='suspect'))reading.className='raw-zero';
  row.append(name,reading);rows.append(row);
 }
 if(!sensors.length)emptyRow(rows,2,query?'无匹配项':state.error?'读取不可用':'暂无读数');
 text('sensor-count',all.length?String(sensors.length):'');text('temperature-heading',`温度 ${state.view.unit==='fahrenheit'?'°F':'°C'}`);
}
function render(){
 const s=state.snapshot,available=s&&s.status!=='unavailable';
 if(s?.model)state.lastModel=s.model;text('model',state.lastModel);
 const fans=available?s.fans:[],rows=$('fan-rows');rows.replaceChildren();
 for(const fan of fans){const row=element('tr');row.append(element('td',fan.name??fan.id),element('td',validRpm(fan)?number(fan.rpm):'—'),element('td',`${number(fan.minRpm)}–${number(fan.maxRpm)}`),element('td',modeName(fan.mode)));row.title=`${fan.id} · 目标 ${number(fan.targetRpm)} RPM · 设备报告${modeName(fan.mode)}模式 · ${fan.modeSource??""} ${fan.modeRaw??""}${fan.evidence?.length?" · 名称按机型资料匹配，未独立校准位置":""}${fan.note?` · ${fan.note}`:""}`;rows.append(row);}
 if(!fans.length)emptyRow(rows,4,(s?.fanCount===0||s?.status==='no_fans')?'设备报告无风扇':state.error?'读取不可用':'暂无风扇读数');
 text('fan-count',fans.length?String(fans.length):'');
 const label=state.error?'离线':({ready:'实时',partial:'部分可用',no_fans:'实时',unavailable:'不可用'}[s?.status]??'连接中');
 text('status-label',demo?`演示 · ${label}`:label);$('status-dot').className=available?(s.status==='partial'?'warning':'live'):'';
 renderControl();
 text('updated',s?new Date(s.capturedAtMs).toLocaleTimeString('zh-CN',{hour12:false}):'');
 text('error-detail',state.error??s?.message??'');
 renderDiagnostics();renderSensors();state.preferences?.snapshotChanged();
}
function renderControl(){
 const controlState=state.control;
 text('status-detail',controlState?.label??'只读');
 $('status-detail').classList.toggle('error',controlState?.label==='未确认'||Boolean(state.error));
 $('status-detail').title=controlState?.message||state.error||'打开诊断';
 renderDiagnostics();
}
function fail(message){state.snapshot=null;state.error=message;render();}
function accept(snapshot){state.snapshot=validateSnapshot(snapshot);state.error=null;render();}
const invoke=window.__TAURI__?.core?.invoke;
const queryDiagnostics=createDiagnosticQuery(invoke,result=>{
 state.service=result.service;state.helper=result.helper;
 state.serviceCompletedAtMs=result.serviceCompletedAtMs;state.helperCompletedAtMs=result.helperCompletedAtMs;
 renderDiagnostics();
});
let tick=0;
function demoSnapshot(){tick++;return{model:'Mac16,8',architecture:'arm64',capturedAtMs:Date.now(),status:'ready',message:'演示数据，不代表本机状态。',controlAvailable:false,fans:[{id:'F0',name:'风扇 1',rpm:2310+Math.round(Math.sin(tick/4)*18),minRpm:2317,maxRpm:7826,targetRpm:2317,mode:'automatic',quality:'valid'},{id:'F1',name:'风扇 2',rpm:2502,minRpm:2317,maxRpm:7826,targetRpm:2502,mode:'automatic',quality:'valid'}],sensors:Array.from({length:16},(_,i)=>({id:`T${i.toString(16).toUpperCase()}0T`,name:['CPU 性能核心','CPU 效能核心','GPU','电池','SSD'][i]??`T${i.toString(16).toUpperCase()}0T`,group:'演示',temperatureC:i===15?0:38+i*.7+Math.sin(tick/5),quality:'valid',source:'DEMO'})),diagnostics:['演示数据']};}
let polling=false,nextPoll;
async function poll(){
 if(polling)return;clearTimeout(nextPoll);polling=true;$('refresh').disabled=true;let timedOut=false;
 const timeout=setTimeout(()=>{timedOut=true;fail('采样超时；等待当前请求结束。');},6000);
 try{const snapshot=demo?demoSnapshot():await invoke('snapshot');if(!timedOut)accept(snapshot);}catch(error){fail(`采样失败：${error instanceof Error?error.message:String(error)}`);}finally{clearTimeout(timeout);polling=false;$('refresh').disabled=false;nextPoll=setTimeout(poll,2000);}
}
$('demo-banner').hidden=!demo;
const connected=demo||typeof invoke==='function';
const control=createControl({invoke:demo?undefined:invoke,onUpdate:value=>{state.control=value;renderControl();state.preferences?.controlChanged();}});
if(!demo&&typeof invoke==='function')control.status();
window.addEventListener('pagehide',()=>control.dispose());
if(connected)poll();else{fail('浏览器无法读取硬件；请打开桌面应用。');$('refresh').disabled=true;}
$('refresh').addEventListener('click',()=>{if(connected)poll();});
setInterval(()=>{const now=Date.now();if(state.snapshot&&now-state.snapshot.capturedAtMs>10000)fail('读数已过期；等待新的采样。');else if(state.snapshot&&sourceFreshnessKey(state.snapshot.status!=='unavailable'?state.snapshot.sensors:[],now)!==renderedSourceFreshness)renderSensors();},1000);
$('sensor-search').addEventListener('input',renderSensors);
state.preferences=setupPreferences({control,invoke:demo?undefined:invoke,getSnapshot:()=>state.snapshot,onView:view=>{state.view={...view};$('sensor-scope').value=view.scope;$('sensor-sort').value=view.sort;renderSensors();}});
$('copy-json').addEventListener('click',async()=>{try{await navigator.clipboard.writeText($('json-output').textContent);text('copy-status','已复制');}catch{const range=document.createRange();range.selectNodeContents($('json-output'));const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);$('json-output').focus();text('copy-status','已选中，按 ⌘C 复制');}});
