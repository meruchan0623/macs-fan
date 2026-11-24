import {evaluateCurve} from './logic.mjs';
import {PreferenceSession,copy,defaultView,draftChanged,createPreset,newMode,validatePreset,presetWarnings,temperatureText} from './preferences-logic.mjs';
const $=id=>document.getElementById(id);
const text=(id,value)=>{$(id).textContent=value;};
const option=(value,label)=>{const o=document.createElement('option');o.value=value;o.textContent=label;return o;};
export function setupPreferences({invoke,getSnapshot,onView,control}){
 const session=new PreferenceSession(invoke);
 const nativeListen=window.__TAURI__?.event?.listen;
 const needsNativeHandshake=typeof invoke==='function'&&typeof nativeListen==='function';
 let draft=null,baseline=null,fanId=null,view=defaultView(),busy=0,error='',pendingDiscard=null,pendingDelete=null;
 const dirty=()=>draftChanged(draft,baseline);
 let nativeExitReady=false,lastNativeDirty=null,nativeSync=Promise.resolve(),approvedExit=false,nativeError='';
 function syncNativeEditor(){
  if(!nativeExitReady)return;
  const value=dirty()||busy>0;if(value===lastNativeDirty)return;lastNativeDirty=value;
  nativeSync=nativeSync.then(()=>invoke('editor_state',{dirty:value})).catch(e=>{lastNativeDirty=null;notify(`退出保护同步失败：${e instanceof Error?e.message:String(e)}`);});
 }
 async function exitAfterDiscard(){
  approvedExit=true;
  try{await nativeSync;await invoke('exit_after_discard');}
  catch(e){approvedExit=false;notify(`无法退出：${e instanceof Error?e.message:String(e)}`);}
 }
 const savedMatchesEditor=()=>{
  const saved=session.document.presets.find(p=>p.id===draft?.id);
  return Boolean(saved&&!dirty()&&!draftChanged(saved,baseline)&&!draftChanged(saved,draft));
 };
 function reconcileEditor(){
  if(!dirty()&&draft?.id)setDraft(session.document.presets.find(p=>p.id===draft.id)??null);
  else{list();warnings();}
 }
 const currentFan=()=>draft?.fans.find(f=>f.fanId===fanId);
 const notify=message=>{
  message=[nativeError,message].filter(Boolean).join('；');
  error=message;text('preset-status',message);text('settings-status',message);
  $('preferences-retry').hidden=!message;$('settings-retry').hidden=!message;
  $('open-settings').classList.toggle('error',Boolean(message));$('open-settings').title=message||'显示设置';
 };
 function enabled(){
  const waiting=busy>0,ready=session.loaded&&(!needsNativeHandshake||nativeExitReady);
  $('preset-fields').disabled=waiting||!ready||!draft;
  $('preset-list').disabled=waiting||!ready;
  $('preset-new').disabled=waiting||!ready||!getSnapshot()?.fans.length||session.document.presets.length>=64;
  $('preset-save').disabled=waiting||!ready||!draft||!dirty();
  $('preset-reset').disabled=waiting||!ready||!draft||!dirty();
  $('preset-delete').disabled=waiting||!ready||!draft?.id;
  const controlState=control.view(),saved=draft?.id&&session.document.presets.some(p=>p.id===draft.id);
  $('preset-apply').disabled=waiting||!ready||!saved||!savedMatchesEditor()||!controlState.canAttemptStart;
  $('preset-stop').disabled=!controlState.canStop;
  text('preset-apply',controlState.startPending?'应用中…':'应用');
  text('preset-stop',controlState.stopPending?'停止中…':'停止');
  text('control-status',controlState.message);
  $('preset-apply').title=dirty()?'先保存修改':!saved?'先保存预设':!savedMatchesEditor()?'预设已更新，请重新读取':controlState.message||'应用已保存预设';
  $('preferences-retry').disabled=waiting;$('settings-retry').disabled=waiting;
  for(const id of ['sensor-scope','sensor-sort','display-unit','display-precision'])$(id).disabled=waiting||!ready;
  text('preset-dirty',dirty()?'未保存':'');if(dirty()&&!error&&!busy)text('preset-status','');
  syncNativeEditor();
 }
 async function operation(fn,recover=false){
  busy++;enabled();
  try{const result=await fn();reconcileEditor();notify('');return result;}
  catch(e){
   let message=e instanceof Error?e.message:String(e);
   if(recover&&session.loaded){
    try{await session.read();reconcileEditor();message+=dirty()?'；已重新读取，未保存的修改仍保留。':'；已重新读取。';}
    catch(readError){message+=`；重新读取失败：${readError instanceof Error?readError.message:String(readError)}`;}
   }
   notify(message);return null;
  }finally{busy--;enabled();}
 }
 function list(){
  const select=$('preset-list');select.replaceChildren(option('',draft&&!draft.id?'新预设':'选择预设'));
  for(const p of session.document.presets)select.append(option(p.id,p.name));
  if(draft?.id&&!session.document.presets.some(p=>p.id===draft.id))select.append(option(draft.id,`${draft.name} · 已被移除`));
  select.value=draft?.id??'';
 }
 function sensors(){
  const f=currentFan();if(f?.mode.type!=='sensor')return;
  const all=getSnapshot()?.sensors??[],select=$('preset-sensor'),selected=f.mode.sensorId;
  select.replaceChildren(option('','选择传感器'));
  if(selected&&!all.some(s=>s.id===selected))select.append(option(selected,`${selected} · 不可用`));
  for(const s of all)select.append(option(s.id,`${s.name??s.id} · ${s.id}${s.quality!=='valid'?' · 无读数':''}`));
  select.value=selected;
 }
 function warnings(){const warnings=presetWarnings(draft,getSnapshot());if(!draft&&!getSnapshot()?.fans.length)warnings.push('等待本机风扇信息后可新建预设。');if(draft?.id&&!session.document.presets.some(p=>p.id===draft.id))warnings.push('此预设已被其他窗口移除；草稿仍保留。');text('preset-warning',warnings.join('；'));}
 function curve(){
  const f=currentFan();if(f?.mode.type!=='sensor'){text('curve-error','');return;}
  const values=f.mode,temperature=Number($('curve-temperature').value),host=$('curve-chart');host.replaceChildren();
  text('curve-temp-label',temperatureText(temperature,view));
  try{
   const rpm=evaluateCurve(values,temperature);text('curve-rpm',rpm.toLocaleString('zh-CN'));text('curve-error','');
   const ns='http://www.w3.org/2000/svg',svg=document.createElementNS(ns,'svg'),path=document.createElementNS(ns,'path');
   svg.setAttribute('viewBox','0 0 360 60');svg.setAttribute('preserveAspectRatio','none');
   path.setAttribute('d',Array.from({length:128},(_,t)=>`${t?'L':'M'}${(t/127*360).toFixed(2)},${(58-evaluateCurve(values,t)/Math.max(values.maxRpm,1)*54).toFixed(2)}`).join(' '));
   path.setAttribute('stroke','currentColor');path.setAttribute('stroke-width','2');path.setAttribute('fill','none');path.setAttribute('vector-effect','non-scaling-stroke');svg.append(path);host.append(svg);
   host.setAttribute('aria-label',`仅预览：${values.lowerTemp}到${values.upperTemp}摄氏度，${values.minRpm}到${values.maxRpm} RPM；当前 ${rpm} RPM`);
  }catch(e){text('curve-rpm','—');text('curve-error',e.message);host.setAttribute('aria-label','参数无效');}
 }
 function editor(){
  $('preset-name').value=draft?.name??'';
  const select=$('preset-fan');select.replaceChildren();
  const live=getSnapshot()?.fans??[];
  for(const f of draft?.fans??[]){const actual=live.find(x=>x.id===f.fanId);select.append(option(f.fanId,actual?`${actual.name??f.fanId} · ${f.fanId}`:`${f.fanId} · 不可用`));}
  if(!draft?.fans.some(f=>f.fanId===fanId))fanId=draft?.fans[0]?.fanId??null;
  select.value=fanId??'';
  const f=currentFan(),type=f?.mode.type??'automatic';$('preset-mode').value=type;
  $('fixed-fields').hidden=type!=='fixed';$('sensor-fields').hidden=type!=='sensor';
  if(type==='fixed')$('fixed-rpm').value=f.mode.rpm;
  if(type==='sensor'){for(const key of ['minRpm','maxRpm','lowerTemp','upperTemp'])$(key).value=f.mode[key];sensors();curve();}else text('curve-error','');
  list();warnings();enabled();
 }
 function setDraft(preset){draft=preset?copy(preset):null;baseline=copy(draft);fanId=draft?.fans[0]?.fanId??null;editor();}
 function guard(action){
  if(busy)return;
  if(!dirty()){action();return;}
  pendingDiscard=action;$('discard-dialog').showModal();
 }
 $('discard-cancel').addEventListener('click',()=>{$('discard-dialog').close();pendingDiscard=null;list();});
 $('discard-dialog').addEventListener('cancel',()=>{pendingDiscard=null;list();});
 $('discard-confirm').addEventListener('click',()=>{const action=pendingDiscard;pendingDiscard=null;$('discard-dialog').close();action?.();});
 function restoreSaved(){setDraft(draft?.id?(session.document.presets.find(p=>p.id===draft.id)??null):baseline);}
 function close(){guard(()=>{restoreSaved();$('curve-dialog').close();});}
 $('curve-dialog').querySelector('[data-close]').addEventListener('click',close);
 $('curve-dialog').addEventListener('cancel',event=>{event.preventDefault();close();});
 window.addEventListener('beforeunload',event=>{if(!approvedExit&&(dirty()||busy)){event.preventDefault();event.returnValue='';}});
 $('open-curve').addEventListener('click',()=>{$('curve-dialog').showModal();editor();});
 $('open-settings').addEventListener('click',()=>$('settings-dialog').showModal());
 $('preset-list').addEventListener('change',()=>{const id=$('preset-list').value;list();guard(async()=>{const doc=await operation(()=>session.update({type:'selectPreset',id:id||null}),true);if(doc)setDraft(doc.presets.find(p=>p.id===doc.editorPresetId)??null);});});
 $('preset-new').addEventListener('click',()=>guard(()=>{try{setDraft(createPreset(getSnapshot()));baseline=null;editor();$('preset-name').focus();}catch(e){notify(e.message);}}));
 $('preset-reset').addEventListener('click',()=>guard(restoreSaved));
 $('preset-name').addEventListener('input',()=>{draft.name=$('preset-name').value;enabled();});
 $('preset-fan').addEventListener('change',()=>{fanId=$('preset-fan').value;editor();});
 $('preset-mode').addEventListener('change',()=>{const f=currentFan();f.mode=newMode($('preset-mode').value,getSnapshot()?.fans.find(x=>x.id===fanId),getSnapshot()?.sensors);editor();});
 $('preset-sensor').addEventListener('change',()=>{currentFan().mode.sensorId=$('preset-sensor').value;warnings();enabled();});
 $('fixed-rpm').addEventListener('input',()=>{currentFan().mode.rpm=$('fixed-rpm').valueAsNumber;warnings();enabled();});
 for(const key of ['minRpm','maxRpm','lowerTemp','upperTemp'])$(key).addEventListener('input',()=>{currentFan().mode[key]=$(key).valueAsNumber;curve();warnings();enabled();});
 $('curve-temperature').addEventListener('input',curve);
 $('preset-form').addEventListener('submit',async event=>{
  event.preventDefault();if(busy||!draft)return;
  let preset;try{preset=validatePreset(draft,session.document.presets);}catch(e){text('curve-error',e.message);return;}
  const doc=await operation(()=>session.update({type:'savePreset',preset}),true);
  if(doc){setDraft(doc.presets.find(p=>p.id===doc.editorPresetId));text('preset-status','已保存');}
 });
 $('preset-apply').addEventListener('click',()=>{
  if(busy||!session.loaded||!draft?.id||!savedMatchesEditor()||!control.view().canAttemptStart)return;
  control.start({presetId:draft.id,expectedRevision:session.document.revision});
 });
 $('preset-stop').addEventListener('click',()=>{if(control.view().canStop)control.stop();});
 $('preset-delete').addEventListener('click',()=>guard(()=>{pendingDelete=draft.id;$('delete-dialog').showModal();}));
 $('delete-cancel').addEventListener('click',()=>{$('delete-dialog').close();pendingDelete=null;});
 $('delete-dialog').addEventListener('cancel',()=>{pendingDelete=null;});
 $('delete-confirm').addEventListener('click',async()=>{const id=pendingDelete;pendingDelete=null;$('delete-dialog').close();if(!id)return;const doc=await operation(()=>session.update({type:'deletePreset',id}),true);if(doc){setDraft(null);text('preset-status','已删除');}});
 function showView(){
  onView(view);$('display-unit').value=view.unit;$('display-precision').value=view.precision;
  if(currentFan()?.mode.type==='sensor')curve();
 }
 for(const [id,key] of [['sensor-scope','scope'],['sensor-sort','sort'],['display-unit','unit'],['display-precision','precision']])$(id).addEventListener('change',async()=>{view={...view,[key]:$(id).value};showView();await operation(()=>session.update({type:'setView',view}),true);});
 async function read(){
  const doc=await operation(()=>session.read());if(!doc)return;
  view=copy(doc.view);showView();
  if(!dirty())setDraft(doc.presets.find(p=>p.id===doc.editorPresetId)??null);
  else{list();text('preset-status','已重新读取；未保存的修改仍保留。');}
 }
 $('preferences-retry').addEventListener('click',read);$('settings-retry').addEventListener('click',read);
 if(needsNativeHandshake){
  nativeListen('editor-exit-requested',()=>{
   if(busy){notify('配置操作仍在进行，请稍后退出。');return;}
   guard(exitAfterDiscard);
  }).then(async()=>{await invoke('editor_state',{dirty:false});lastNativeDirty=false;nativeExitReady=true;enabled();})
    .catch(e=>{nativeError=`退出保护不可用：${e instanceof Error?e.message:String(e)}；请退出并重新打开应用。`;notify('');});
 }
 enabled();list();read();
 return {controlChanged(){enabled();},snapshotChanged(){warnings();sensors();enabled();}};
}
