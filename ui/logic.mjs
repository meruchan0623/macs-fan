export function evaluateCurve({minRpm,maxRpm,lowerTemp,upperTemp}, temperature) {
  if (![minRpm,maxRpm,lowerTemp,upperTemp,temperature].every(Number.isFinite)) throw new Error('所有参数必须是有效数字。');
  if (![minRpm,maxRpm].every(Number.isInteger) || minRpm < 0 || maxRpm > 30000 || minRpm >= maxRpm) throw new Error('转速须为 0–30000 的整数，且最低转速小于最高转速。');
  if (lowerTemp < 0 || upperTemp > 127 || lowerTemp >= upperTemp) throw new Error('温度须在 0–127 °C 之间，且起调温度低于满速温度。');
  if (temperature < 0 || temperature > 127) throw new Error('预览温度须在 0–127 °C 之间。');
  return Math.round(minRpm + (maxRpm-minRpm) * Math.max(0,Math.min(1,(temperature-lowerTemp)/(upperTemp-lowerTemp))));
}
// Once either field is present, malformed timing must never use the legacy path.
export function sensorFreshness(sensor, now = Date.now()) {
  if (!sensor || !('sampledAtMs' in sensor || 'expiresAtMs' in sensor)) return 'legacy';
  const {sampledAtMs,expiresAtMs}=sensor;
  if (![sampledAtMs,expiresAtMs,now].every(Number.isSafeInteger) || sampledAtMs < 0 ||
      expiresAtMs <= sampledAtMs || expiresAtMs-sampledAtMs > 45000 || sampledAtMs > now) return 'invalid';
  return now < expiresAtMs ? 'fresh' : 'expired';
}
export const validTemperature = (s, now = Date.now()) => s?.quality === 'valid' && Number.isFinite(s.temperatureC) && s.temperatureC >= -40 && s.temperatureC <= 150 && ['legacy','fresh'].includes(sensorFreshness(s,now));
export function nextSensorExpiry(sensors, now = Date.now()) {
  const deadlines=(sensors ?? []).filter(s => validTemperature(s,now) && sensorFreshness(s,now)==='fresh').map(s=>s.expiresAtMs);
  return deadlines.length ? Math.min(...deadlines) : null;
}
export const validRpm = f => f?.quality === 'valid' && Number.isFinite(f.rpm) && f.rpm >= 0 && f.rpm <= 30000;
export function validateSnapshot(s, now = Date.now()) {
  if (!s || !['ready','partial','unavailable','no_fans'].includes(s.status) || !Array.isArray(s.fans) || !Array.isArray(s.sensors) || !Number.isFinite(s.capturedAtMs)) throw new Error('采样格式无效，请查看诊断。');
  if (s.capturedAtMs > now+5000 || now-s.capturedAtMs > 10000) throw new Error('采样已过期，已隐藏实时读数。');
  return s;
}


// Classification describes evidence for a label, not measurement calibration.
export function selectSensors(sensors, scope = 'overview', query = '', now = Date.now()) {
  const all = Array.isArray(sensors) ? sensors : [];
  const hasMapped = all.some(s => ['mapped', 'derived'].includes(s.classification));
  // A newly discovered but unreadable source must not suppress the useful raw fallback.
  const hasUsablePrimary = all.some(s => s.primary === true && validTemperature(s,now) &&
    s.temperatureC !== 0 && s.assessment !== 'suspect');
  const scoped = !hasMapped || scope === 'all' || (scope === 'overview' && !hasUsablePrimary) ? all : all.filter(s =>
    scope === 'components' ? ['mapped', 'derived'].includes(s.classification) : s.primary === true);
  const needle = query.trim().toLocaleLowerCase();
  return scoped.filter(s => !needle || [s.name, s.id, s.source, s.group, ...(Array.isArray(s.aliases) ? s.aliases : [])]
    .some(v => String(v ?? '').toLocaleLowerCase().includes(needle)));
}
export function sensorDetail(sensor, now = Date.now()) {
  const freshness=sensorFreshness(sensor,now);
  const sample=freshness==='legacy' ? '' : ['fresh','expired'].includes(freshness)
    ? `采样于 ${new Date(sensor.sampledAtMs).toLocaleString('zh-CN',{hour12:false})}${freshness==='expired' ? '；读数已过期' : ''}` : '采样时刻无效';
  const identity = sensor.classification === 'derived' ? '按声明成员计算'
    : sensor.classification === 'mapped' ? '资料映射；测量位置未独立校准' : '原始候选；部件含义未确认';
  return [sensor.source ?? '来源未知', identity, sensor.interpretation,
    Array.isArray(sensor.aliases) && sensor.aliases.length ? `同源别名：${sensor.aliases.join('、')}` : '',
    sample,
    validTemperature(sensor,now) ? (freshness==='legacy' ? '本次已解码' : '已解码') : sensor.quality === 'invalid' ? '读数无效' : '读取不可用',
    sensor.temperatureC === 0 ? '0°C可能为占位值' : ''].filter(Boolean).join(' · ');
}
