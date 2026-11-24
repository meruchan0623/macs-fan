// Native calls remain in flight after a presentation deadline. Stop has its own
// lane so cancellation can reach the service while start is awaiting a reply.
const STATES=new Set(['idle','blocked','preparing','active','recovery_pending','automatic_confirmed','unknown']);
const OPERATIONS={start:'start_plan',stop:'stop_plan',status:'control_status'};
export function validControlResponse(value,kind){
 return value?.schema_version===1&&value.operation===OPERATIONS[kind]&&STATES.has(value.state)
  &&typeof value.reason==='string'&&['hardware_write_enabled','authenticated_response','current','authorization_live','request_in_flight','can_attempt_start','can_stop'].every(k=>typeof value[k]==='boolean')
  &&(value.last_response_age_ms===null||(Number.isFinite(value.last_response_age_ms)&&value.last_response_age_ms>=0));
}
const messages={production_team_unconfigured:'控制服务未配置',readonly_service:'当前服务只读',readonly_policy:'当前服务只读',no_reviewed_profile:'此机型暂不可调速',profile_validation_failed:'风扇配置校验失败',backend_open_failed:'无法连接风扇设备',backend_unavailable:'风扇设备不可用',authorization_denied:'未获控制授权',authorization_required:'需要控制授权',preferences_revision_conflict:'预设已更新，请重新读取',control_timeout:'响应超时',control_unavailable:'控制不可用',invalid_control_response:'控制响应无效'};
export function createControl({invoke,onUpdate=()=>{},timeoutMs=4000,pollMs=750,newRequestId=()=>globalThis.crypto.randomUUID(),now=()=>globalThis.performance.now(),setTimer=setTimeout,clearTimer=clearTimeout}={}){
 const available=typeof invoke==='function',slots={start:null,stop:null,status:null};
 let generation=0,response=null,receivedAt=null,error='',lastFailure=null,uncertain=false,disposed=false,pollTimer=null,ageTimer=null;
 let clockInvalid=false,lastClock=null;
 function view(){
  const clockNow=now();
  if(!Number.isFinite(clockNow)||(lastClock!==null&&clockNow<lastClock))clockInvalid=true;
  if(Number.isFinite(clockNow))lastClock=clockNow;
  const elapsed=receivedAt===null?Infinity:clockNow-receivedAt;
  const freshReceipt=!clockInvalid&&Number.isFinite(elapsed)&&elapsed>=0&&elapsed<=1500;
  const age=clockInvalid||elapsed<0||!Number.isFinite(elapsed)||response?.last_response_age_ms===null?Infinity:(response?.last_response_age_ms??Infinity)+elapsed;
  const observed=Boolean(response?.authenticated_response&&response.current&&age<=1500);
  const verified=observed&&response.hardware_write_enabled;
  const active=verified&&response.authorization_live&&response.state==='active'&&!uncertain;
  const preparing=verified&&response.authorization_live&&response.state==='preparing'&&response.plan?.authorized===true&&response.plan?.phase==='preparing'&&Array.isArray(response.plan?.fans)&&response.plan.fans.length>0&&response.plan.fans.every(f=>['unobserved','automatic_confirmed'].includes(f.state))&&!uncertain;
  // A confirmed stop closes the writable connection; its fresh readback
  // remains evidence of automatic mode without retaining write capability.
  const automatic=observed&&response.state==='automatic_confirmed'&&!uncertain;
  const recoveryPending=response?.state==='recovery_pending';
  const recovering=verified&&recoveryPending&&!uncertain;
  const unconfirmed=uncertain||(!active&&!automatic&&!preparing&&['preparing','active','automatic_confirmed','recovery_pending','unknown'].includes(response?.state));
  const label=preparing?'准备中':recovering?'恢复中':active?'控制中':unconfirmed?'未确认':'只读';
  return {label,active,automatic,preparing,canAttemptStart:available&&!slots.start&&!slots.stop&&!uncertain&&Boolean(response?.can_attempt_start&&freshReceipt),
   canStop:available&&!slots.stop&&Boolean(slots.start||uncertain||response?.can_stop||active||response?.state==='preparing'||recoveryPending),startPending:Boolean(slots.start),stopPending:Boolean(slots.stop),
   message:error||(response?.state==='blocked'?(messages[response.reason]??'控制不可用'):''),
   diagnostic:{response,receivedAtMs:receivedAt,clockInvalid,effectiveResponseAgeMs:Number.isFinite(age)?age:null,generation,inFlight:Object.fromEntries(Object.entries(slots).map(([key,value])=>[key,Boolean(value)])),error,lastFailure,uncertain}};
 }
 function emit(){if(!disposed)onUpdate(view());}
 function pollLater(){if(disposed||!available||pollTimer!==null)return;pollTimer=setTimer(()=>{pollTimer=null;status();},pollMs);}
 function request(kind,args){
  if(!available||disposed)return Promise.resolve(null);
  if(slots[kind])return slots[kind].visible;
  if(kind==='start'&&(slots.stop||!view().canAttemptStart))return Promise.resolve(null);
  if(kind==='status'&&(slots.start||slots.stop)){pollLater();return Promise.resolve(null);}
  const epoch=kind==='status'?generation:++generation;
  let done,visibleDone=false,expired=false;
  const visible=new Promise(resolve=>{done=resolve;});
  if(kind==='start')args={...args,requestId:newRequestId()};
  if(kind==='stop')args={cancelRequestId:slots.start?.requestId??null};
  const entry={visible,requestId:kind==='start'?args.requestId:null};slots[kind]=entry;
  const applicable=()=>!expired&&epoch===generation&&(kind!=='status'||(!slots.start&&!slots.stop));
  const finish=value=>{if(!visibleDone){visibleDone=true;done(value);}};
  if(kind!=='status'){uncertain=true;error='';}
  emit();
  const timer=setTimer(()=>{
   const wasApplicable=applicable();expired=true;
   if(wasApplicable){uncertain=true;error='响应超时';emit();}
   finish(null);
  },timeoutMs);
  Promise.resolve().then(()=>invoke({start:'control_start',stop:'control_stop',status:'control_status'}[kind],args)).then(value=>{
   if(applicable()){
    if(validControlResponse(value,kind)){response=value;receivedAt=now();clockInvalid=!Number.isFinite(receivedAt);lastClock=clockInvalid?null:receivedAt;error='';uncertain=false;}
    else{uncertain=true;error=messages.invalid_control_response;}
    emit();
   }
   finish(value);
  },failure=>{
   if(applicable()){uncertain=true;error='控制请求失败';response=null;receivedAt=null;lastFailure=String(failure).slice(0,2048);emit();}
   finish(null);
  }).finally(()=>{
   clearTimer(timer);if(slots[kind]===entry)slots[kind]=null;
   emit();
   if(expired){clearTimer(pollTimer);pollTimer=null;status();}else pollLater();
  });
  return visible;
 }
 function status(){return request('status');}
 ageTimer=setTimer(function tick(){if(disposed)return;emit();ageTimer=setTimer(tick,250);},250);
 emit();
 return {view,status,start:({presetId,expectedRevision})=>request('start',{presetId,expectedRevision}),stop:()=>request('stop'),
  dispose(){disposed=true;clearTimer(pollTimer);clearTimer(ageTimer);},
 };
}
