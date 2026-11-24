//! Explicit saved-preset execution. UI input never supplies hardware commands,
//! owner identity, capability claims, temperatures, or lease credentials.
use crate::{monitor::Snapshot, preferences};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REQUEST_IDS: usize = 65_536;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_ENTRIES_BYTES: usize = 3500;

#[derive(Default)]
struct IntentState {
    serial: u64,
    requests: HashMap<String, bool>,
    pending: Option<String>,
    reject_starts: bool,
}
static INTENTS: LazyLock<Mutex<IntentState>> = LazyLock::new(|| Mutex::new(IntentState::default()));

fn valid_request_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
impl IntentState {
    fn begin(&mut self, request_id: &str) -> Result<u64, String> {
        if !valid_request_id(request_id) {
            return Err("控制请求 ID 无效".into());
        }
        if self.requests.contains_key(request_id) {
            return Err("控制请求已取消或已处理".into());
        }
        if self.reject_starts
            || self.requests.len() >= MAX_REQUEST_IDS
            || self.serial >= u64::MAX - 1
        {
            return Err("控制会话已耗尽，请退出并重新打开应用".into());
        }
        if self.pending.is_some() {
            return Err("启动请求仍在处理".into());
        }
        self.serial += 1;
        self.requests.insert(request_id.to_owned(), false);
        self.pending = Some(request_id.to_owned());
        Ok(self.serial)
    }
    fn cancel(&mut self, request_id: Option<&str>) -> Result<u64, String> {
        if request_id.is_some_and(|id| !valid_request_id(id)) {
            return Err("取消请求 ID 无效".into());
        }
        if let Some(pending) = &self.pending {
            self.requests.insert(pending.clone(), true);
        }
        if let Some(id) = request_id {
            if self.requests.contains_key(id) || self.requests.len() < MAX_REQUEST_IDS {
                self.requests.insert(id.to_owned(), true);
            } else {
                self.reject_starts = true;
            }
        }
        // MAX is reserved for an emergency stop, never a start. Stop remains
        // available when request-ID memory or the ordinary sequence is exhausted.
        self.serial = self.serial.saturating_add(1);
        Ok(self.serial)
    }
    fn current(&self, request_id: &str, serial: u64) -> bool {
        self.serial == serial
            && self.pending.as_deref() == Some(request_id)
            && self.requests.get(request_id) == Some(&false)
    }
}

pub struct StartTicket {
    id: String,
    serial: u64,
}
impl Drop for StartTicket {
    fn drop(&mut self) {
        if let Ok(mut state) = INTENTS.lock() {
            if state.pending.as_deref() == Some(&self.id) {
                state.pending = None;
            }
        }
    }
}
pub fn reserve_start(request_id: &str) -> Result<StartTicket, String> {
    let serial = INTENTS
        .lock()
        .map_err(|_| "控制意图状态已失效")?
        .begin(request_id)?;
    Ok(StartTicket {
        id: request_id.to_owned(),
        serial,
    })
}
pub fn reserve_stop(cancel_request_id: Option<&str>) -> Result<u64, String> {
    match INTENTS.lock() {
        Ok(mut state) => state.cancel(cancel_request_id),
        // A poisoned local ledger must not make the independent native stop
        // unavailable. Starts fail closed on this same poisoned mutex.
        Err(_) => Ok(u64::MAX),
    }
}
impl StartTicket {
    fn is_current(&self) -> bool {
        INTENTS
            .lock()
            .is_ok_and(|state| state.current(&self.id, self.serial))
    }
}

fn fan_id(text: &str) -> Result<u16, String> {
    let b = text.as_bytes();
    if b.len() != 2 || b[0] != b'F' {
        return Err("风扇 ID 无效".into());
    }
    match b[1] {
        b'0'..=b'9' => Ok(u16::from(b[1] - b'0')),
        b'A'..=b'F' => Ok(u16::from(b[1] - b'A' + 10)),
        _ => Err("风扇 ID 无效".into()),
    }
}
fn now_ms() -> Result<u64, String> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间不可用")?
        .as_millis();
    u64::try_from(value).map_err(|_| "系统时间超出范围".into())
}

fn compile_entries(
    document: &preferences::Document,
    preset_id: &str,
    expected_revision: u64,
    snapshot: &Snapshot,
    now: u64,
) -> Result<Vec<u8>, String> {
    if expected_revision > MAX_SAFE_INTEGER || document.revision != expected_revision {
        return Err("预设已更新，请重新读取".into());
    }
    let preset = document
        .presets
        .iter()
        .find(|p| p.id == preset_id)
        .ok_or("预设不存在，请重新读取")?;
    if preset.model != snapshot.model || preset.architecture != snapshot.architecture {
        return Err("预设机型或架构与本机不匹配".into());
    }
    if !matches!(snapshot.status.as_str(), "ready" | "partial")
        || now < snapshot.captured_at_ms
        || now - snapshot.captured_at_ms > 5000
    {
        return Err("风扇读数不可用或已过期".into());
    }
    if snapshot.fans.is_empty()
        || snapshot.fans.len() > 16
        || snapshot.fan_count != Some(snapshot.fans.len() as u32)
        || preset.fans.len() != snapshot.fans.len()
    {
        return Err("风扇清单已变化，请重新创建预设".into());
    }
    let mut inventory = HashMap::new();
    for fan in &snapshot.fans {
        let (id, min, max) = fan.control_observation();
        let id = fan_id(id)?;
        if inventory.insert(id, (min, max)).is_some() {
            return Err("设备返回重复风扇".into());
        }
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for fan in &preset.fans {
        let id = fan_id(&fan.fan_id)?;
        if !seen.insert(id) {
            return Err("预设有重复风扇".into());
        }
        let &(min, max) = inventory.get(&id).ok_or("预设风扇已不可用")?;
        let in_range = |rpm: u32| -> bool {
            matches!((min,max),(Some(low),Some(high)) if low.is_finite() && high.is_finite() && low>=0.0 && high<=30000.0 && low<=high && (low..=high).contains(&f64::from(rpm)))
        };
        let strategy = match &fan.mode {
            preferences::Mode::Automatic {} => json!({"type":"automatic"}),
            preferences::Mode::Fixed { rpm } => {
                if !in_range(*rpm) {
                    return Err("固定转速超出本机读回范围".into());
                }
                json!({"type":"fixed","rpm":rpm})
            }
            preferences::Mode::Sensor {
                sensor_id,
                min_rpm,
                max_rpm,
                lower_temp,
                upper_temp,
            } => {
                if !in_range(*min_rpm) || !in_range(*max_rpm) || min_rpm >= max_rpm {
                    return Err("温控转速范围无效".into());
                }
                if !lower_temp.is_finite()
                    || !upper_temp.is_finite()
                    || *lower_temp < 0.0
                    || *upper_temp > 120.0
                    || lower_temp >= upper_temp
                {
                    return Err("温控阈值超出执行策略范围".into());
                }
                if !snapshot.sensors.iter().any(|s| s.stable_id() == sensor_id) {
                    return Err("预设温度传感器已不可用".into());
                }
                // This is an identifier only. Temperature, quality, time and
                // hardware admission remain exclusively the helper's authority.
                json!({"type":"sensor","sensor_id":sensor_id,"min_rpm":min_rpm,"max_rpm":max_rpm,"lower_temp":lower_temp,"upper_temp":upper_temp})
            }
        };
        entries.push(json!({"fan":id,"strategy":strategy}));
    }
    let bytes = serde_json::to_vec(&entries).map_err(|_| "无法编码控制计划")?;
    if bytes.len() > MAX_ENTRIES_BYTES {
        return Err("控制计划过长".into());
    }
    Ok(bytes)
}

fn decode(bytes: &[u8], operation: &str) -> Result<Value, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("控制响应过长".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "控制响应格式无效")?;
    let fields = [
        "schema_version",
        "operation",
        "state",
        "reason",
        "hardware_write_enabled",
        "authenticated_response",
        "current",
        "last_response_age_ms",
        "plan",
        "authorization_live",
        "request_in_flight",
        "can_attempt_start",
        "can_stop",
    ];
    let object = value.as_object().ok_or("控制响应格式无效")?;
    if object.len() != fields.len()
        || fields.iter().any(|f| !object.contains_key(*f))
        || value["schema_version"].as_u64() != Some(1)
        || value["operation"].as_str() != Some(operation)
    {
        return Err("控制响应协议不匹配".into());
    }
    if !matches!(
        value["state"].as_str(),
        Some(
            "idle"
                | "blocked"
                | "preparing"
                | "active"
                | "recovery_pending"
                | "automatic_confirmed"
                | "unknown"
        )
    ) {
        return Err("控制状态无效".into());
    }
    if !value["reason"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 128)
    {
        return Err("控制原因无效".into());
    }
    for key in [
        "hardware_write_enabled",
        "authenticated_response",
        "current",
        "authorization_live",
        "request_in_flight",
        "can_attempt_start",
        "can_stop",
    ] {
        if !value[key].is_boolean() {
            return Err("控制标记无效".into());
        }
    }
    if !(value["last_response_age_ms"].is_null()
        || value["last_response_age_ms"].as_u64().is_some())
        || !(value["plan"].is_null() || value["plan"].is_object())
    {
        return Err("控制观察无效".into());
    }
    fn has_secret(v: &Value) -> bool {
        match v {
            Value::Object(m) => m
                .iter()
                .any(|(k, v)| matches!(k.as_str(), "token" | "owner") || has_secret(v)),
            Value::Array(a) => a.iter().any(has_secret),
            _ => false,
        }
    }
    if has_secret(&value) {
        return Err("控制响应包含禁止字段".into());
    }
    if value["authorization_live"] == true
        && (value["current"] != true || value["authenticated_response"] != true)
    {
        return Err("控制授权状态不一致".into());
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::ffi::{c_char, CStr};
    extern "C" {
        fn mf_control_start(entries: *const u8, size: usize, intent: u64) -> *mut c_char;
        fn mf_control_status() -> *mut c_char;
        fn mf_control_stop(intent: u64) -> *mut c_char;
        fn mf_control_free(value: *mut c_char);
    }
    struct Owned(*mut c_char);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { mf_control_free(self.0) }
        }
    }
    pub fn request(operation: &str, entries: Option<&[u8]>, intent: u64) -> Result<Value, String> {
        // SAFETY: fixed native ABI; entry buffer lives through the call and
        // native copies it. Each non-null returned CString is freed exactly once.
        let response = Owned(unsafe {
            match operation {
                "start_plan" => {
                    let bytes = entries.ok_or("控制计划缺失")?;
                    mf_control_start(bytes.as_ptr(), bytes.len(), intent)
                }
                "stop_plan" => mf_control_stop(intent),
                "control_status" => mf_control_status(),
                _ => return Err("控制操作无效".into()),
            }
        });
        if response.0.is_null() {
            return Err("控制客户端未返回响应".into());
        }
        decode(unsafe { CStr::from_ptr(response.0) }.to_bytes(), operation)
    }
}
#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;
    pub fn request(operation: &str, _: Option<&[u8]>, _: u64) -> Result<Value, String> {
        Ok(
            json!({"schema_version":1,"operation":operation,"state":"blocked","reason":"macos_required","hardware_write_enabled":false,"authenticated_response":false,"current":false,"last_response_age_ms":null,"plan":null,"authorization_live":false,"request_in_flight":false,"can_attempt_start":false,"can_stop":false}),
        )
    }
}

pub fn status() -> Result<Value, String> {
    let mut response = native::request("control_status", None, 0)?;
    if INTENTS.lock().is_ok_and(|s| s.pending.is_some()) {
        response["request_in_flight"] = json!(true);
        response["can_stop"] = json!(true);
        response["can_attempt_start"] = json!(false);
    }
    Ok(response)
}
pub fn stop(intent: u64) -> Result<Value, String> {
    native::request("stop_plan", None, intent)
}

pub fn start(
    ticket: StartTicket,
    store: preferences::Store,
    preset_id: String,
    expected_revision: u64,
    sample: impl FnOnce() -> Result<Snapshot, String>,
) -> Result<Value, String> {
    if !ticket.is_current() {
        return Err("控制请求已取消".into());
    }
    let mut gate = native::request("control_status", None, 0)?;
    if gate["can_attempt_start"] != true {
        gate["operation"] = json!("start_plan");
        return Ok(gate);
    }
    let snapshot = sample()?;
    if !ticket.is_current() {
        return Err("控制请求已取消".into());
    }
    let document = store.read()?;
    let entries = compile_entries(
        &document,
        &preset_id,
        expected_revision,
        &snapshot,
        now_ms()?,
    )?;
    if !ticket.is_current() {
        return Err("控制请求已取消".into());
    }
    // Native intent ordering closes the final check-to-call race with stop.
    native::request("start_plan", Some(&entries), ticket.serial)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-a111-111111111111";
    const OTHER: &str = "22222222-2222-4222-a222-222222222222";
    fn fixture() -> (preferences::Document, Snapshot) {
        let mut doc = preferences::Document {
            revision: 7,
            ..Default::default()
        };
        doc.presets.push(preferences::Preset {
            id: "preset".into(),
            name: "Test".into(),
            model: "Mac16,8".into(),
            architecture: "aarch64".into(),
            fans: vec![preferences::Fan {
                fan_id: "F0".into(),
                mode: preferences::Mode::Fixed { rpm: 2000 },
            }],
        });
        let snap = serde_json::from_value(json!({
            "model":"Mac16,8","architecture":"aarch64","capturedAtMs":1000,"status":"partial","message":"","controlAvailable":false,
            "fanCount":1,"fans":[{"id":"F0","name":"Fan","rpm":1800,"minRpm":1000,"maxRpm":6000,"targetRpm":null,"mode":"unknown","quality":"valid"}],
            "sensors":[{"id":"Tp01","name":"CPU","group":"CPU","temperatureC":55,"quality":"valid","source":"SMC"}],"diagnostics":[]
        })).unwrap();
        (doc, snap)
    }
    fn run(doc: &preferences::Document, snap: &Snapshot) -> Result<Vec<u8>, String> {
        compile_entries(doc, "preset", 7, snap, 1000)
    }
    #[test]
    fn stop_before_start_and_duplicate_requests_never_reenter() {
        let mut state = IntentState::default();
        assert_eq!(state.cancel(Some(ID)).unwrap(), 1);
        assert!(state.begin(ID).is_err());
        let serial = state.begin(OTHER).unwrap();
        assert!(state.current(OTHER, serial));
        assert!(state.begin(OTHER).is_err());
        state.cancel(None).unwrap();
        assert!(!state.current(OTHER, serial));
        state.pending = None;
        assert!(state.begin(OTHER).is_err());
    }
    #[test]
    fn invalid_cancel_does_not_mutate_pending_intent() {
        let mut state = IntentState::default();
        let serial = state.begin(ID).unwrap();
        assert!(state.begin(OTHER).is_err());
        assert!(state.cancel(Some("invalid")).is_err());
        assert!(state.current(ID, serial));
    }
    #[test]
    fn stop_remains_available_at_sequence_and_memory_limits() {
        let mut state = IntentState {
            serial: u64::MAX - 1,
            ..Default::default()
        };
        assert!(state.begin(ID).is_err());
        assert_eq!(state.cancel(None).unwrap(), u64::MAX);
        assert_eq!(state.cancel(None).unwrap(), u64::MAX);
        state = IntentState::default();
        for i in 0..MAX_REQUEST_IDS {
            state.requests.insert(format!("{i}"), false);
        }
        assert!(state.begin(ID).is_err());
        state.cancel(Some(ID)).unwrap();
        assert!(state.reject_starts);
        assert_eq!(state.requests.len(), MAX_REQUEST_IDS);
        state.requests.clear();
        assert!(state.begin(ID).is_err());
    }
    #[test]
    fn compile_only_closed_configuration_never_samples_or_capabilities() {
        let (mut doc, snap) = fixture();
        assert_eq!(
            serde_json::from_slice::<Value>(&run(&doc, &snap).unwrap()).unwrap(),
            json!([{"fan":0,"strategy":{"type":"fixed","rpm":2000}}])
        );
        doc.presets[0].fans[0].mode = preferences::Mode::Sensor {
            sensor_id: "Tp01".into(),
            min_rpm: 1000,
            max_rpm: 6000,
            lower_temp: 40.0,
            upper_temp: 80.0,
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&run(&doc, &snap).unwrap()).unwrap(),
            json!([{"fan":0,"strategy":{"type":"sensor","sensor_id":"Tp01","min_rpm":1000,"max_rpm":6000,"lower_temp":40.0,"upper_temp":80.0}}])
        );
    }
    #[test]
    fn compile_rejects_revision_machine_freshness_and_inventory_changes() {
        let (doc, mut snap) = fixture();
        assert!(compile_entries(&doc, "preset", 6, &snap, 1000).is_err());
        assert!(compile_entries(&doc, "missing", 7, &snap, 1000).is_err());
        assert!(compile_entries(&doc, "preset", 7, &snap, 999).is_err());
        assert!(compile_entries(&doc, "preset", 7, &snap, 6000).is_ok());
        assert!(compile_entries(&doc, "preset", 7, &snap, 6001).is_err());
        snap.model = "Other".into();
        assert!(run(&doc, &snap).is_err());
        snap.model = doc.presets[0].model.clone();
        snap.architecture = "x86_64".into();
        assert!(run(&doc, &snap).is_err());
        snap.architecture = doc.presets[0].architecture.clone();
        snap.status = "unavailable".into();
        assert!(run(&doc, &snap).is_err());
        snap.status = "ready".into();
        snap.fan_count = None;
        assert!(run(&doc, &snap).is_err());
        snap.fan_count = Some(2);
        assert!(run(&doc, &snap).is_err());
    }
    #[test]
    fn compile_rejects_missing_invalid_duplicate_and_out_of_range_fans() {
        let (mut doc, mut snap) = fixture();
        for id in ["F1", "F00", "f0", "FG"] {
            doc.presets[0].fans[0].fan_id = id.into();
            assert!(run(&doc, &snap).is_err());
        }
        doc.presets[0].fans[0].fan_id = "F0".into();
        for rpm in [999, 6001] {
            doc.presets[0].fans[0].mode = preferences::Mode::Fixed { rpm };
            assert!(run(&doc, &snap).is_err());
        }
        doc.presets[0].fans[0].mode = preferences::Mode::Automatic {};
        let duplicate =
            serde_json::from_value(serde_json::to_value(&snap.fans[0]).unwrap()).unwrap();
        snap.fans.push(duplicate);
        snap.fan_count = Some(2);
        let duplicate = doc.presets[0].fans[0].clone();
        doc.presets[0].fans.push(duplicate);
        assert!(run(&doc, &snap).is_err());
    }
    #[test]
    fn sensor_configuration_requires_present_id_and_executor_bounds() {
        let (mut doc, snap) = fixture();
        for (id, low, high, min, max) in [
            ("missing", 40.0, 80.0, 1000, 6000),
            ("Tp01", 40.0, 121.0, 1000, 6000),
            ("Tp01", 80.0, 80.0, 1000, 6000),
            ("Tp01", f64::NAN, 80.0, 1000, 6000),
            ("Tp01", 40.0, 80.0, 2000, 2000),
            ("Tp01", 40.0, 80.0, 999, 6000),
        ] {
            doc.presets[0].fans[0].mode = preferences::Mode::Sensor {
                sensor_id: id.into(),
                min_rpm: min,
                max_rpm: max,
                lower_temp: low,
                upper_temp: high,
            };
            assert!(run(&doc, &snap).is_err());
        }
    }
    fn response() -> Value {
        json!({"schema_version":1,"operation":"control_status","state":"blocked","reason":"production_team_unconfigured","hardware_write_enabled":false,"authenticated_response":false,"current":false,"last_response_age_ms":null,"plan":null,"authorization_live":false,"request_in_flight":false,"can_attempt_start":false,"can_stop":false})
    }
    #[test]
    fn native_boundary_rejects_secret_wrong_operation_and_inconsistent_flags() {
        let base = response();
        assert!(decode(&serde_json::to_vec(&base).unwrap(), "control_status").is_ok());
        assert!(decode(&serde_json::to_vec(&base).unwrap(), "start_plan").is_err());
        for (key, value) in [
            ("extra", json!(0)),
            ("can_stop", json!(1)),
            ("authorization_live", json!(true)),
            ("last_response_age_ms", json!(-1)),
            ("plan", json!({"fans":[{"token":"secret"}]})),
        ] {
            let mut changed = base.clone();
            changed[key] = value;
            assert!(decode(&serde_json::to_vec(&changed).unwrap(), "control_status").is_err());
        }
    }
}
