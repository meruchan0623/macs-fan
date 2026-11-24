import {evaluateCurve} from './logic.mjs';
export const defaultView=()=>({scope:'overview',sort:'name',unit:'celsius',precision:'decimal'});
export const emptyDocument=()=>({schemaVersion:1,revision:0,presets:[],editorPresetId:null,view:defaultView()});
export const copy=value=>JSON.parse(JSON.stringify(value));
export const draftChanged=(draft,baseline)=>JSON.stringify(draft)!==JSON.stringify(baseline);
export function temperatureText(celsius,view=defaultView(),suffix=true){
 if(!Number.isFinite(celsius))return '—';
 const value=view.unit==='fahrenheit'?celsius*9/5+32:celsius;
 return `${value.toFixed(view.precision==='whole'?0:1)}${suffix?view.unit==='fahrenheit'?' °F':' °C':''}`;
}
export function createPreset(snapshot){
 if(!snapshot?.model||!snapshot.architecture||!snapshot.fans?.length)throw new Error('等待本机风扇信息后再新建。');
 return {id:null,name:'',model:snapshot.model,architecture:snapshot.architecture,fans:snapshot.fans.map(f=>({fanId:f.id,mode:{type:'automatic'}}))};
}
export function newMode(type,fan,sensors=[]){
 if(type==='automatic')return {type};
 const min=Number.isInteger(fan?.minRpm)?fan.minRpm:0,max=Number.isInteger(fan?.maxRpm)?fan.maxRpm:30000;
 if(type==='fixed')return {type,rpm:min};
 if(type==='sensor')return {type,sensorId:sensors[0]?.id??'',minRpm:min,maxRpm:max,lowerTemp:45,upperTemp:85};
 throw new Error('未知模式。');
}
const bytes=value=>new TextEncoder().encode(value).length;
export function validatePreset(preset,presets=[]){
 const name=preset.name.trim();
 if(!name||[...name].length>64||bytes(name)>256||/\p{Cc}/u.test(name))throw new Error('名称须为 1–64 个字符，且不能含控制字符。');
 if(presets.some(p=>p.id!==preset.id&&p.name.toLowerCase()===name.toLowerCase()))throw new Error('已有同名预设。');
 if(!/^[A-Za-z0-9,]{1,64}$/.test(preset.model)||!/^[A-Za-z0-9_]{1,32}$/.test(preset.architecture))throw new Error('机型或架构信息无效。');
 if(!Array.isArray(preset.fans)||!preset.fans.length||preset.fans.length>16)throw new Error('预设须包含 1–16 个风扇。');
 const ids=new Set();
 for(const fan of preset.fans){
  if(!/^F[0-9A-F]$/.test(fan.fanId)||ids.has(fan.fanId))throw new Error('风扇标识无效或重复。');ids.add(fan.fanId);
  const m=fan.mode;
  if(m?.type==='automatic')continue;
  if(m?.type==='fixed'){
   if(!Number.isInteger(m.rpm)||m.rpm<0||m.rpm>30000)throw new Error('固定转速须为 0–30000 的整数。');
  }else if(m?.type==='sensor'){
   if(typeof m.sensorId!=='string'||!m.sensorId||bytes(m.sensorId)>256||/\p{Cc}/u.test(m.sensorId))throw new Error('请选择有效的温度传感器。');
   evaluateCurve(m,65);
  }else throw new Error('风扇模式无效。');
 }
 return {...copy(preset),name};
}
export function presetWarnings(preset,snapshot){
 if(!preset)return [];
 if(!snapshot)return ['当前硬件不可用；无法核对预设。'];
 const result=[];
 if(preset.model!==snapshot.model||preset.architecture!==snapshot.architecture)result.push(`其他硬件：${preset.model} / ${preset.architecture}`);
 for(const f of preset.fans){
  const live=snapshot.fans.find(x=>x.id===f.fanId);
  if(!live)result.push(`${f.fanId} 当前不可用`);
  if(f.mode.type==='sensor'&&!snapshot.sensors.some(s=>s.id===f.mode.sensorId))result.push(`${f.mode.sensorId||'未选传感器'} 当前不可用`);
  const values=f.mode.type==='fixed'?[f.mode.rpm]:f.mode.type==='sensor'?[f.mode.minRpm,f.mode.maxRpm]:[];
  if(live&&values.some(r=>Number.isFinite(r)&&(r<live.minRpm||r>live.maxRpm)))result.push(`${f.fanId} 超出本机范围 ${live.minRpm}–${live.maxRpm} RPM`);
 }
 return result;
}
// A failed write never replaces the committed document. Every queued write uses
// the revision produced by the preceding successful write, including after errors.
export class PreferenceSession{
 constructor(invoke){this.invoke=invoke;this.document=emptyDocument();this.queue=Promise.resolve();this.loaded=false;}
 run(operation){const task=this.queue.then(operation);this.queue=task.catch(()=>{});return task;}
 read(){return this.run(async()=>{if(typeof this.invoke!=='function')throw new Error('浏览器无法保存配置；请打开桌面应用。');const doc=await this.invoke('preferences_read');this.document=doc;this.loaded=true;return doc;});}
 update(update){const value=copy(update);return this.run(async()=>{if(!this.loaded)throw new Error('配置尚未读取；请重新读取后保存。');const doc=await this.invoke('preferences_update',{expectedRevision:this.document.revision,update:value});this.document=doc;return doc;});}
}
