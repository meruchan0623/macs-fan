// A UI deadline does not cancel a native operation. Keep each underlying request
// until it settles so repeated opens cannot accumulate blocked native calls.
export function createDiagnosticQuery(invoke,onUpdate=()=>{},timeoutMs=4000){
 const active={service:null,helper:null};
 const commands={service:'service_status',helper:'helper_inspect'};
 let pending=null;
 function read(key){
  if(active[key])return active[key].visible;
  let resolve,finished=false;
  const visible=new Promise(done=>{resolve=done;});
  const entry={visible};active[key]=entry;
  const finish=value=>{
   if(finished)return;
   finished=true;resolve({value,completedAtMs:Date.now()});
  };
  const timer=setTimeout(()=>finish({result:'unavailable',reason:'diagnostic_query_timeout'}),timeoutMs);
  Promise.resolve().then(()=>invoke(commands[key])).then(
   value=>finish(value),
   error=>finish({result:'unavailable',reason:String(error).slice(0,2048)})
  ).finally(()=>{clearTimeout(timer);if(active[key]===entry)active[key]=null;});
  return visible;
 }
 return ()=>{
  if(pending)return pending;
  pending=Promise.resolve().then(async()=>{
   const state={service:null,helper:null,serviceCompletedAtMs:null,helperCompletedAtMs:null};
   onUpdate({...state});
   await Promise.all(Object.keys(commands).map(async key=>{
    const result=await read(key);
    state[key]=result.value;state[`${key}CompletedAtMs`]=result.completedAtMs;
    onUpdate({...state});
   }));
   return{...state};
  }).finally(()=>{pending=null;});
  return pending;
 };
}
