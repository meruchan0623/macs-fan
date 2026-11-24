use crate::smc::{self, Connection, KeyInfo};
use crate::{catalog, disk, disk_cache, fan_catalog, gpu, hid, hid_catalog};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub model: String,
    pub architecture: String,
    pub captured_at_ms: u64,
    pub status: String,
    pub message: String,
    pub control_available: bool,
    #[serde(default)]
    pub fan_count: Option<u32>,
    pub fans: Vec<Fan>,
    pub sensors: Vec<Sensor>,
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub providers: Vec<ProviderStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    #[serde(default)]
    cached: bool,
    id: String,
    status: String,
    discovered: usize,
    decoded: usize,
    captured_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fan {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<SensorEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    rpm: Option<f64>,
    min_rpm: Option<f64>,
    max_rpm: Option<f64>,
    target_rpm: Option<f64>,
    mode: String,
    #[serde(default)]
    mode_source: String,
    #[serde(default)]
    mode_raw: Option<u32>,
    quality: String,
}

impl Fan {
    pub fn control_observation(&self) -> (&str, Option<f64>, Option<f64>) {
        (&self.id, self.min_rpm, self.max_rpm)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sensor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sampled_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<u64>,
    id: String,
    name: String,
    group: String,
    temperature_c: Option<f64>,
    quality: String,
    source: String,
    #[serde(default)]
    classification: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    assessment: String,
    #[serde(default)]
    interpretation: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    evidence: Vec<SensorEvidence>,
}

impl Sensor {
    pub fn stable_id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SensorEvidence {
    file: String,
    line: u16,
    model_scope: String,
}

fn evidence_for(descriptor: &catalog::Descriptor) -> Vec<SensorEvidence> {
    descriptor
        .evidence
        .iter()
        .map(|e| SensorEvidence {
            file: e.file.into(),
            line: e.line,
            model_scope: e.model_scope.into(),
        })
        .collect()
}

/// Labels and aggregates are evidence-based interpretations, separate from raw decoding.
fn annotate_sensors(snapshot: &mut Snapshot) -> bool {
    let descriptions = catalog::catalogue_for_architecture(&snapshot.architecture, &snapshot.model);
    for sensor in &mut snapshot.sensors {
        let Some(d) = descriptions.iter().find(|d| {
            !d.is_aggregate()
                && catalog::keys_match_for_architecture(&snapshot.architecture, &d.id, &sensor.id)
        }) else {
            sensor.classification = catalog::Classification::Raw.as_str().into();
            // The UI owns the shared raw explanation; do not repeat it in IPC.
            sensor.interpretation.clear();
            continue;
        };
        sensor.name = d.display_name.clone();
        sensor.aliases = d.aliases.clone();
        sensor.group = d.group.into();
        sensor.classification = "mapped".into();
        sensor.primary =
            !matches!(d.group, "CPU" | "GPU") || matches!(sensor.id.as_str(), "TCDX" | "TCMb");
        sensor.evidence = evidence_for(d);
        sensor.interpretation = d
            .note
            .unwrap_or("按适用机型资源声明匹配，未独立校准测量位置")
            .into();
        // The original correction only targets Tp values below 11. Preserve the value;
        // flag this known suspicious condition instead of substituting or randomizing it.
        if sensor.temperature_c.is_some_and(|v| {
            v == 0.0
                || (matches!(snapshot.architecture.as_str(), "aarch64" | "arm64")
                    && sensor.id.starts_with("Tp")
                    && v < 11.0)
        }) {
            sensor.assessment = "suspect".into();
            sensor
                .interpretation
                .push_str("；原始低值可能为休眠核心或占位，未补算且不参与均值");
        }
    }
    let mut incomplete = false;
    for d in descriptions.iter().filter(|d| d.is_aggregate()) {
        let intel = matches!(snapshot.architecture.as_str(), "x86_64" | "x86" | "i386");
        let mut ambiguous = false;
        let members: Vec<String> = if intel {
            d.member_ids
                .iter()
                .flat_map(|declared| {
                    let matching: Vec<_> = snapshot
                        .sensors
                        .iter()
                        .filter(|sensor| {
                            sensor.classification == "mapped"
                                && catalog::keys_match_for_architecture(
                                    &snapshot.architecture,
                                    declared,
                                    &sensor.id,
                                )
                        })
                        .map(|sensor| sensor.id.clone())
                        .collect();
                    ambiguous |= matching.len() > 1;
                    matching
                })
                .collect()
        } else {
            d.member_ids.clone()
        };
        // Intel all-model groups are candidates, not a promised CPU core count.
        if members.is_empty() {
            continue;
        }
        let values: Vec<f64> = members
            .iter()
            .filter_map(|id| {
                snapshot
                    .sensors
                    .iter()
                    .find(|sensor| sensor.id == *id && sensor.classification == "mapped")
                    .filter(|sensor| sensor.quality == "valid" && sensor.assessment != "suspect")
                    .and_then(|sensor| sensor.temperature_c)
                    .filter(|value| value.is_finite() && *value > 0.0 && *value <= 150.0)
            })
            .collect();
        let complete = values.len() == members.len() && !values.is_empty() && !ambiguous;
        incomplete |= !complete;
        let temperature_c = complete.then(|| values.iter().sum::<f64>() / values.len() as f64);
        snapshot.sensors.push(Sensor {
            sampled_at_ms: None,
            expires_at_ms: None,
            id: d.id.clone(),
            name: d.display_name.clone(),
            group: d.group.into(),
            temperature_c,
            quality: if complete { "valid" } else { "unavailable" }.into(),
            source: format!("AppleSMC 派生 · {}", members.join(", ")),
            classification: "derived".into(),
            primary: true,
            assessment: if complete { "derived" } else { "incomplete" }.into(),
            interpretation: format!(
                "本次可用成员 {}/{}；{}；{}",
                values.len(),
                members.len(),
                if intel {
                    "按已发现成员计算，不代表完整物理核心数"
                } else {
                    "要求全部声明成员有效"
                },
                if ambiguous {
                    "大小写源匹配有歧义，均值不可用"
                } else {
                    "不补算缺失或可疑值"
                }
            ),
            aliases: d.aliases.clone(),
            members,
            evidence: evidence_for(d),
        });
    }
    for group in ["CPU", "GPU"] {
        let has_summary = snapshot
            .sensors
            .iter()
            .any(|s| s.group == group && s.classification == "derived" && s.quality == "valid");
        if !has_summary {
            for sensor in snapshot.sensors.iter_mut().filter(|s| {
                s.group == group
                    && s.classification == "mapped"
                    && s.quality == "valid"
                    && s.assessment != "suspect"
            }) {
                sensor.primary = true;
            }
        }
    }
    if let Some(e) = catalog::normalization_evidence(&snapshot.model) {
        snapshot.diagnostics.push(format!(
            "机型归一化依据 {}:{} ({})",
            e.file, e.line, e.model_scope
        ));
    }
    let mapped = snapshot
        .sensors
        .iter()
        .filter(|s| s.classification == "mapped")
        .count();
    let suspect = snapshot
        .sensors
        .iter()
        .filter(|s| s.assessment == "suspect")
        .count();
    snapshot.diagnostics.push(format!("机型{}，归一化{}；{mapped}个键按资料映射，{suspect}个映射读数可疑；其余仅为原始候选，映射不代表测量校准或控制授权", snapshot.model, catalog::normalize_model(&snapshot.model)));
    incomplete
}

/// Merge an independent provider without allowing an SMC connection failure to hide it.
fn merge_hid(snapshot: &mut Snapshot, batch: hid::Batch) {
    let mut incomplete = !batch.available || batch.truncated;
    let mut products: HashMap<&str, usize> = HashMap::new();
    for reading in &batch.readings {
        if let Some(product) = reading.product.as_deref() {
            *products.entry(product).or_default() += 1;
        }
    }
    let mut hid_sensors = Vec::new();
    let mut group_sources: HashMap<&str, Vec<String>> = HashMap::new();
    let mut ambiguous_groups = std::collections::HashSet::new();
    for reading in &batch.readings {
        let fact = reading.product.as_deref().and_then(hid_catalog::lookup);
        let duplicate = reading
            .product
            .as_deref()
            .is_some_and(|product| products[product] > 1);
        let (quality, assessment) = match reading.quality {
            hid::Quality::Valid => (
                "valid",
                if fact.is_some() {
                    "mapped"
                } else {
                    "unverified"
                },
            ),
            hid::Quality::Suspect => ("valid", "suspect"),
            hid::Quality::Invalid => (reading.quality.as_str(), "invalid"),
            hid::Quality::Unavailable => (reading.quality.as_str(), "unavailable"),
        };
        incomplete |= quality != "valid" || assessment == "suspect";
        let mut name = fact.map(|fact| fact.name.to_string()).unwrap_or_else(|| {
            reading
                .product
                .clone()
                .unwrap_or_else(|| "未命名HID源".into())
        });
        if duplicate {
            name.push_str(&format!(" · {:X}", reading.registry_id));
        }
        if let Some(average) = fact.and_then(|fact| fact.average) {
            group_sources
                .entry(average)
                .or_default()
                .push(reading.source_id.clone());
            if duplicate {
                ambiguous_groups.insert(average);
            }
        }
        let mut interpretation = if fact.is_some() {
            "按Product资料匹配；未独立校准测量位置".to_string()
        } else {
            "未识别Product，保留独立服务的原始读数".to_string()
        };
        if duplicate {
            interpretation.push_str("；同名服务有多个registry ID，未合并且不生成其组均值");
        }
        if let Some(message) = &reading.message {
            interpretation.push('；');
            interpretation.push_str(message);
        }
        hid_sensors.push(Sensor {
            sampled_at_ms: None,
            expires_at_ms: None,
            id: reading.source_id.clone(),
            name,
            group: fact.map_or("HID 原始源", |fact| fact.group).into(),
            temperature_c: reading.temperature_c,
            quality: quality.into(),
            source: format!(
                "IOKit HID · {} · registry {:X}",
                reading.product.as_deref().unwrap_or("无Product"),
                reading.registry_id
            ),
            classification: if fact.is_some() { "mapped" } else { "raw" }.into(),
            primary: fact.is_some_and(|fact| fact.average.is_none()) && !duplicate,
            assessment: assessment.into(),
            interpretation,
            aliases: Vec::new(),
            members: Vec::new(),
            evidence: fact
                .map(|fact| {
                    vec![SensorEvidence {
                        file: hid_catalog::SOURCE.into(),
                        line: fact.line,
                        model_scope: "all".into(),
                    }]
                })
                .unwrap_or_default(),
        });
    }
    let decoded = hid_sensors
        .iter()
        .filter(|sensor| sensor.quality == "valid")
        .count();
    for average in hid_catalog::AVERAGES {
        let Some(members) = group_sources.get(average.id) else {
            continue;
        };
        let values: Vec<_> = members
            .iter()
            .filter_map(|id| {
                hid_sensors
                    .iter()
                    .find(|sensor| sensor.id == *id)
                    .filter(|sensor| sensor.quality == "valid" && sensor.assessment != "suspect")
                    .and_then(|sensor| sensor.temperature_c)
                    .filter(|value| value.is_finite() && *value > 0.0 && *value <= 150.0)
            })
            .collect();
        let ambiguous = ambiguous_groups.contains(average.id);
        let complete = !ambiguous && !members.is_empty() && values.len() == members.len();
        incomplete |= !complete;
        if !complete {
            for sensor in hid_sensors
                .iter_mut()
                .filter(|sensor| members.contains(&sensor.id))
            {
                sensor.primary =
                    !ambiguous && sensor.quality == "valid" && sensor.assessment != "suspect";
            }
        }
        hid_sensors.push(Sensor {
            sampled_at_ms: None,
            expires_at_ms: None,
            id: average.id.into(),
            name: average.name.into(),
            group: average.group.into(),
            temperature_c: complete.then(|| values.iter().sum::<f64>() / values.len() as f64),
            quality: if complete { "valid" } else { "unavailable" }.into(),
            source: format!("IOKit HID 派生 · {}个源", members.len()),
            classification: "derived".into(),
            primary: true,
            assessment: if complete { "derived" } else { "incomplete" }.into(),
            interpretation: format!(
                "已发现成员本次可用 {}/{}；{}；不是完整芯片核心数或SMC均值",
                values.len(),
                members.len(),
                if ambiguous {
                    "同名服务歧义，均值不可用"
                } else {
                    "只在全部已发现成员有效时计算"
                }
            ),
            aliases: Vec::new(),
            members: members.clone(),
            evidence: vec![SensorEvidence {
                file: hid_catalog::SOURCE.into(),
                line: average.line,
                model_scope: "all".into(),
            }],
        });
    }
    snapshot.providers.push(ProviderStatus {
        cached: false,
        id: "hid".into(),
        status: if !batch.available {
            "unavailable"
        } else if incomplete {
            "partial"
        } else if batch.readings.is_empty() {
            "empty"
        } else {
            "ready"
        }
        .into(),
        discovered: batch.service_count,
        decoded,
        captured_at_ms: batch.captured_at_ms,
    });
    snapshot.diagnostics.extend(batch.diagnostics);
    snapshot.sensors.extend(hid_sensors);
    let any_reading = snapshot.fans.iter().any(|fan| fan.rpm.is_some())
        || snapshot
            .sensors
            .iter()
            .any(|sensor| sensor.temperature_c.is_some());
    if any_reading && (snapshot.status == "unavailable" || incomplete) {
        snapshot.status = "partial".into();
        snapshot.message = "部分来源或读数不可用；有效读数来自本次采样".into();
    }
}

// A read seam keeps failure transitions testable without touching hardware.
/// Independent registry readings never replace, average with, or authorize an SMC source.
fn merge_gpu(snapshot: &mut Snapshot, batch: gpu::Batch) {
    let mut incomplete = batch.supported && (!batch.available || batch.truncated);
    let mut decoded = 0;
    let reading_count = batch.readings.len();
    let mut names: HashMap<String, usize> = HashMap::new();
    for reading in &batch.readings {
        *names
            .entry(reading.name.clone().unwrap_or_else(|| "GPU".into()))
            .or_default() += 1;
    }
    if batch.supported {
        for reading in batch.readings {
            let (quality, assessment) = match reading.quality {
                gpu::Quality::Valid => (reading.quality.as_str(), "unverified"),
                gpu::Quality::Suspect => ("valid", "suspect"),
                gpu::Quality::Invalid => (reading.quality.as_str(), "invalid"),
                gpu::Quality::Unavailable => (reading.quality.as_str(), "unavailable"),
            };
            incomplete |= quality != "valid" || assessment == "suspect";
            decoded += usize::from(quality == "valid");
            let mut name = reading.name.unwrap_or_else(|| "GPU".into());
            if names[&name] > 1 {
                name.push_str(&format!(" · {:X}", reading.registry_id));
            }
            let mut interpretation =
                "GPU节点的Temperature(C)属性；未校准测量位置，未与SMC合并".to_string();
            if let Some(message) = reading.message {
                interpretation.push('；');
                interpretation.push_str(&message);
            }
            snapshot.sensors.push(Sensor {
                sampled_at_ms: None,
                expires_at_ms: None,
                id: reading.source_id,
                name,
                group: "GPU".into(),
                temperature_c: reading.temperature_c,
                quality: quality.into(),
                source: format!(
                    "IORegistry · PerformanceStatistics/Temperature(C) · registry {:X}",
                    reading.registry_id
                ),
                classification: "mapped".into(),
                primary: true,
                assessment: assessment.into(),
                interpretation,
                aliases: Vec::new(),
                members: Vec::new(),
                evidence: vec![SensorEvidence {
                    file: "docs/gpu-provider.md".into(),
                    line: 1,
                    model_scope: "x86_64 GPU registry；未做Intel实机验证".into(),
                }],
            });
        }
    }
    snapshot.providers.push(ProviderStatus {
        cached: false,
        id: "gpu_registry".into(),
        status: if !batch.supported {
            "not_applicable"
        } else if !batch.available {
            "unavailable"
        } else if incomplete {
            "partial"
        } else if reading_count == 0 {
            "empty"
        } else {
            "ready"
        }
        .into(),
        discovered: batch.service_count,
        decoded,
        captured_at_ms: batch.captured_at_ms,
    });
    snapshot.diagnostics.extend(batch.diagnostics);
    let any_reading = snapshot.fans.iter().any(|fan| fan.rpm.is_some())
        || snapshot
            .sensors
            .iter()
            .any(|sensor| sensor.temperature_c.is_some());
    if any_reading && (snapshot.status == "unavailable" || incomplete) {
        snapshot.status = "partial".into();
        snapshot.message = "部分来源或读数不可用；有效读数来自本次采样".into();
    }
}

fn merge_disk(snapshot: &mut Snapshot, batch: disk::Batch, cached: bool, observed_at_ms: u64) {
    let expires_at_ms = batch.captured_at_ms.checked_add(disk_cache::DISPLAY_TTL_MS);
    let current = batch.captured_at_ms <= observed_at_ms
        && expires_at_ms.is_some_and(|deadline| observed_at_ms < deadline);
    let mut incomplete = batch.supported && (!batch.available || batch.truncated || !current);
    let reading_count = batch.readings.len();
    let mut decoded = 0;
    let mut names: HashMap<String, usize> = HashMap::new();
    for reading in &batch.readings {
        *names
            .entry(reading.name.clone().unwrap_or_else(|| "磁盘".into()))
            .or_default() += 1;
    }
    if batch.supported {
        for reading in batch.readings {
            let (quality, assessment) = if !current {
                ("stale", "stale")
            } else if reading.temperature_c.is_none()
                && matches!(
                    reading.quality,
                    disk::Quality::Valid | disk::Quality::Suspect
                )
            {
                ("invalid", "suspect")
            } else {
                match reading.quality {
                    disk::Quality::Valid => (reading.quality.as_str(), "unverified"),
                    disk::Quality::Suspect => ("valid", "suspect"),
                    disk::Quality::Invalid => (reading.quality.as_str(), "invalid"),
                    disk::Quality::Unavailable => (reading.quality.as_str(), "unavailable"),
                }
            };
            incomplete |= quality != "valid" || assessment == "suspect";
            decoded += usize::from(quality == "valid");
            let mut name = reading.name.unwrap_or_else(|| "磁盘".into());
            if names[&name] > 1 {
                name.push_str(&format!(" · {:X}", reading.registry_id));
            }
            let mut interpretation =
                "SMART候选读数；每30秒采样，45秒过期；未校准且不用于控制".to_string();
            if let Some(message) = reading.message {
                interpretation.push('；');
                interpretation.push_str(&message);
            }
            snapshot.sensors.push(Sensor {
                sampled_at_ms: Some(batch.captured_at_ms),
                expires_at_ms,
                id: reading.source_id,
                name,
                group: "Disk".into(),
                temperature_c: if current { reading.temperature_c } else { None },
                quality: quality.into(),
                source: format!(
                    "{} SMART · registry {:X}",
                    reading.transport, reading.registry_id
                ),
                classification: "mapped".into(),
                primary: true,
                assessment: assessment.into(),
                interpretation,
                aliases: Vec::new(),
                members: Vec::new(),
                evidence: vec![SensorEvidence {
                    file: "docs/disk-provider.md".into(),
                    line: 1,
                    model_scope: "IOBlockStorageDevice SMART capability；候选而非校准".into(),
                }],
            });
        }
    }
    snapshot.providers.push(ProviderStatus {
        cached,
        id: "disk".into(),
        status: if !batch.supported {
            "not_applicable"
        } else if !batch.available {
            "unavailable"
        } else if incomplete {
            "partial"
        } else if reading_count == 0 {
            "empty"
        } else {
            "ready"
        }
        .into(),
        discovered: batch.service_count,
        decoded,
        captured_at_ms: batch.captured_at_ms,
    });
    snapshot.diagnostics.extend(batch.diagnostics);
    if batch.supported && !current {
        snapshot
            .diagnostics
            .push("磁盘样本已过期或时钟变化；未更新缓存采样时刻".into());
    }
    if cached && reading_count > 0 && snapshot.status == "partial" {
        snapshot.message = "部分来源或读数不可用；各来源采样时刻见详情".into();
    }
    let any_reading = snapshot.fans.iter().any(|fan| fan.rpm.is_some())
        || snapshot
            .sensors
            .iter()
            .any(|sensor| sensor.temperature_c.is_some());
    if any_reading && (snapshot.status == "unavailable" || incomplete) {
        snapshot.status = "partial".into();
        snapshot.message = "部分来源或读数不可用；各来源采样时刻见详情".into();
    }
}

trait Reader {
    fn number(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<f64, String>;
    fn info(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<KeyInfo, String>;
    fn key_at(&self, index: u32) -> Result<String, String>;
}

impl Reader for Connection {
    fn number(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<f64, String> {
        Connection::number(self, key, cache)
    }
    fn info(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<KeyInfo, String> {
        Connection::info(self, key, cache)
    }
    fn key_at(&self, index: u32) -> Result<String, String> {
        Connection::key_at(self, index)
    }
}

#[derive(Default)]
pub struct Monitor {
    disk_sampler: disk_cache::Sampler,
    metadata: HashMap<String, KeyInfo>,
    temperature_keys: Option<Vec<String>>,
    discovery_diagnostics: Vec<String>,
    directory_scanned_at: Option<Instant>,
    directory_had_failures: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(target_os = "macos")]
fn hardware_model() -> Result<String, String> {
    use std::ffi::{c_char, c_void};
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            old_len: *mut usize,
            new: *mut c_void,
            new_len: usize,
        ) -> i32;
    }
    let mut length = 0;
    let result = unsafe {
        sysctlbyname(
            c"hw.model".as_ptr(),
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length == 0 || length > 256 {
        return Err("sysctl hw.model 长度查询失败".into());
    }
    let mut bytes = vec![0u8; length];
    let result = unsafe {
        sysctlbyname(
            c"hw.model".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length == 0 || length > bytes.len() {
        return Err("sysctl hw.model 读取失败".into());
    }
    bytes.truncate(length);
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    String::from_utf8(bytes).map_err(|_| "hw.model 编码无效".into())
}

#[cfg(not(target_os = "macos"))]
fn hardware_model() -> Result<String, String> {
    Err("hw.model 仅支持 macOS".into())
}

fn integer(value: f64, max: u32) -> Result<u32, String> {
    if value.is_finite() && value >= 0.0 && value <= max as f64 && value.fract() == 0.0 {
        Ok(value as u32)
    } else {
        Err(format!("无效整数读数 {value}（允许0…{max}）"))
    }
}

impl Snapshot {
    fn empty() -> Self {
        let model = hardware_model();
        let diagnostics = model.as_ref().err().cloned().into_iter().collect();
        Self {
            model: model.unwrap_or_else(|_| "未知机型".into()),
            architecture: std::env::consts::ARCH.into(),
            captured_at_ms: now_ms(),
            status: "unavailable".into(),
            message: "正在读取 AppleSMC".into(),
            control_available: false,
            fan_count: None,
            fans: Vec::new(),
            sensors: Vec::new(),
            diagnostics,
            providers: Vec::new(),
        }
    }
}

impl Monitor {
    pub fn snapshot(&mut self) -> Snapshot {
        let mut snapshot = Snapshot::empty();
        match Connection::open() {
            Ok(connection) => self.collect(&connection, &mut snapshot),
            Err(error) => {
                self.temperature_keys = None;
                self.metadata.clear();
                self.directory_scanned_at = None;
                snapshot.message = "当前无法访问 AppleSMC，未取得实时读数".into();
                snapshot.diagnostics.push(error);
            }
        }
        snapshot.providers.push(ProviderStatus {
            cached: false,
            id: "smc".into(),
            status: snapshot.status.clone(),
            discovered: snapshot
                .sensors
                .iter()
                .filter(|sensor| sensor.classification != "derived")
                .count(),
            decoded: snapshot
                .sensors
                .iter()
                .filter(|sensor| sensor.classification != "derived" && sensor.quality == "valid")
                .count(),
            captured_at_ms: snapshot.captured_at_ms,
        });
        merge_hid(&mut snapshot, hid::sample());
        merge_gpu(&mut snapshot, gpu::sample());
        let (disk_batch, cached) = self.disk_sampler.sample();
        merge_disk(&mut snapshot, disk_batch, cached, now_ms());
        // Use the start of this read: slow discovery must not make early values look fresh.
        snapshot
    }

    fn discover(&mut self, reader: &impl Reader) -> Result<(), String> {
        let refresh_after = Duration::from_secs(if self.directory_had_failures { 60 } else { 300 });
        if self.temperature_keys.is_some()
            && self
                .directory_scanned_at
                .is_none_or(|at| at.elapsed() < refresh_after)
        {
            return Ok(());
        }
        self.temperature_keys = None;
        self.metadata.clear();
        self.discovery_diagnostics.clear();
        let count = reader
            .number("#KEY", &mut self.metadata)
            .and_then(|n| integer(n, u32::MAX))?;
        let mut keys = Vec::new();
        let mut failed = 0;
        let mut unsupported = 0;
        for index in 0..count.min(4096) {
            match reader.key_at(index) {
                Ok(key) if key.starts_with('T') => match reader.info(&key, &mut self.metadata) {
                    Ok(info) if matches!(smc::type_name(info.kind).as_str(), "flt " | "sp78") => {
                        keys.push(key)
                    }
                    Ok(_) => unsupported += 1,
                    Err(_) => failed += 1,
                },
                Ok(_) => {}
                Err(_) => failed += 1,
            }
        }
        keys.sort();
        keys.dedup();
        self.discovery_diagnostics.clear();
        if count > 4096 {
            self.discovery_diagnostics
                .push(format!("SMC 共{count}个键，安全上限仅枚举前4096个"));
        }
        if failed > 0 {
            self.discovery_diagnostics.push(format!(
                "首次键目录发现有{failed}项读取失败，目录可能不完整"
            ));
        }
        if unsupported > 0 {
            self.discovery_diagnostics.push(format!(
                "已排除{unsupported}个非 flt/sp78 的 T 前缀键，未推断为温度"
            ));
        }
        // Cache only a successful directory scan; a total transport failure is retried.
        if count > 0 && failed == count.min(4096) {
            return Err("SMC 键目录全部读取失败".into());
        }
        self.temperature_keys = Some(keys);
        self.directory_scanned_at = Some(Instant::now());
        self.directory_had_failures = failed > 0;
        Ok(())
    }

    fn rpm(
        &mut self,
        reader: &impl Reader,
        key: &str,
        diagnostics: &mut Vec<String>,
    ) -> Option<f64> {
        match reader.number(key, &mut self.metadata) {
            Ok(n) if n.is_finite() && (0.0..=30000.0).contains(&n) => Some(n),
            Ok(n) => {
                diagnostics.push(format!("{key}：无效转速 {n}"));
                None
            }
            Err(error) => {
                diagnostics.push(format!("{key}：{error}"));
                None
            }
        }
    }

    fn mode(&mut self, reader: &impl Reader, index: u32) -> Result<(String, String, u32), String> {
        // Follow the observed protocol selection: a present FS! mask takes precedence.
        // Once selected, a failed mask read must not be hidden by a contradictory direct key.
        if let Ok(info) = reader.info("FS! ", &mut self.metadata) {
            if info.size != 2 || smc::type_name(info.kind) != "ui16" {
                return Err("FS! 模式掩码编码或尺寸未知".into());
            }
            let mask = reader
                .number("FS! ", &mut self.metadata)
                .and_then(|v| integer(v, 65535))?;
            let mode = if mask & (1 << index) == 0 {
                "automatic"
            } else {
                "manual"
            };
            return Ok((mode.into(), "FS! ".into(), mask));
        }
        let mut direct = Vec::new();
        for key in [format!("F{index:X}Md"), format!("F{index:X}md")] {
            let Ok(info) = reader.info(&key, &mut self.metadata) else {
                continue;
            };
            if info.size != 1 || smc::type_name(info.kind) != "ui8 " {
                return Err(format!("{key} 模式编码或尺寸未知"));
            }
            let raw = reader
                .number(&key, &mut self.metadata)
                .and_then(|v| integer(v, 255))?;
            direct.push((key, raw));
        }
        let Some((key, raw)) = direct.first() else {
            return Err("未找到可读取的模式键".into());
        };
        if direct.iter().any(|(_, other)| other != raw) {
            return Ok((
                "unknown".into(),
                format!(
                    "模式键冲突：{}",
                    direct
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                *raw,
            ));
        }
        // Nonzero bool-casting does not establish firmware semantics for 2/3/etc.
        let mode = match raw {
            0 => "automatic",
            1 => "manual",
            _ => "unknown",
        };
        Ok((mode.into(), key.clone(), *raw))
    }

    fn collect(&mut self, reader: &impl Reader, snapshot: &mut Snapshot) {
        // Read fan count first: absence and a successful zero have different meanings.
        let fan_count = reader
            .number("FNum", &mut self.metadata)
            .and_then(|n| integer(n, 16));
        snapshot.fan_count = fan_count.as_ref().ok().copied();
        let mut incomplete = false;
        match &fan_count {
            Ok(count) => {
                for index in 0..*count {
                    let prefix = format!("F{index:X}");
                    let rpm = self.rpm(reader, &format!("{prefix}Ac"), &mut snapshot.diagnostics);
                    let min_rpm =
                        self.rpm(reader, &format!("{prefix}Mn"), &mut snapshot.diagnostics);
                    let max_rpm =
                        self.rpm(reader, &format!("{prefix}Mx"), &mut snapshot.diagnostics);
                    let target_rpm =
                        self.rpm(reader, &format!("{prefix}Tg"), &mut snapshot.diagnostics);
                    let (mode, mode_source, mode_raw) = self
                        .mode(reader, index)
                        .map(|(mode, key, raw)| (mode, key, Some(raw)))
                        .unwrap_or_else(|error| {
                            snapshot.diagnostics.push(format!("{prefix} 模式：{error}"));
                            ("unknown".into(), String::new(), None)
                        });
                    incomplete |= rpm.is_none()
                        || min_rpm.is_none()
                        || max_rpm.is_none()
                        || target_rpm.is_none()
                        || mode == "unknown";
                    let (min_rpm, max_rpm) =
                        if min_rpm.zip(max_rpm).is_some_and(|(min, max)| min > max) {
                            snapshot
                                .diagnostics
                                .push(format!("{prefix} 的最小转速高于最大转速，范围已作废"));
                            incomplete = true;
                            (None, None)
                        } else {
                            (min_rpm, max_rpm)
                        };
                    let descriptor = fan_catalog::describe(&snapshot.model, index);
                    snapshot.fans.push(Fan {
                        id: prefix,
                        name: descriptor.name,
                        evidence: descriptor
                            .evidence
                            .iter()
                            .map(|e| SensorEvidence {
                                file: e.file.into(),
                                line: e.line,
                                model_scope: e.model_scope.into(),
                            })
                            .collect(),
                        note: descriptor.note.map(str::to_owned),
                        rpm,
                        min_rpm,
                        max_rpm,
                        target_rpm,
                        mode,
                        mode_source,
                        mode_raw,
                        quality: if rpm.is_some() {
                            "valid"
                        } else {
                            "unavailable"
                        }
                        .into(),
                    });
                }
            }
            Err(error) => {
                incomplete = true;
                snapshot
                    .diagnostics
                    .push(format!("FNum：{error}；不能确认风扇数量"));
            }
        }
        if let Err(error) = self.discover(reader) {
            incomplete = true;
            snapshot
                .diagnostics
                .push(format!("温度键发现失败：{error}"));
        }
        snapshot
            .diagnostics
            .extend(self.discovery_diagnostics.iter().cloned());
        incomplete |= self
            .discovery_diagnostics
            .iter()
            .any(|note| note.contains("失败") || note.contains("上限"));
        for key in self.temperature_keys.as_deref().unwrap_or_default() {
            let kind = self
                .metadata
                .get(key)
                .map(|info| smc::type_name(info.kind))
                .unwrap_or_default();
            let (temperature_c, quality) = match reader.number(key, &mut self.metadata) {
                Ok(n) if n.is_finite() && (-40.0..=150.0).contains(&n) => (Some(n), "valid"),
                Ok(n) => {
                    snapshot
                        .diagnostics
                        .push(format!("{key}：温度 {n}°C 超出显示范围 -40…150°C"));
                    (None, "invalid")
                }
                Err(error) => {
                    if snapshot.diagnostics.len() < 24 {
                        snapshot.diagnostics.push(format!("{key}：{error}"));
                    }
                    (None, "unavailable")
                }
            };
            incomplete |= quality != "valid";
            snapshot.sensors.push(Sensor {
                sampled_at_ms: None,
                expires_at_ms: None,
                id: key.clone(),
                name: key.clone(),
                group: "SMC 原始键".into(),
                temperature_c,
                quality: quality.into(),
                source: format!("AppleSMC · {key} · {}", kind.trim()),
                classification: "raw".into(),
                primary: false,
                assessment: "unverified".into(),
                interpretation: String::new(),
                aliases: Vec::new(),
                members: Vec::new(),
                evidence: Vec::new(),
            });
        }
        if snapshot.sensors.is_empty() {
            incomplete = true;
            snapshot
                .diagnostics
                .push("未发现可解码的温度键；不代表设备没有温度传感器".into());
        } else {
            snapshot
                .diagnostics
                .push("保留原始 SMC 读数；不对低温读数补值或随机修正".into());
            let zero_count = snapshot
                .sensors
                .iter()
                .filter(|sensor| sensor.temperature_c == Some(0.0))
                .count();
            if zero_count > 0 {
                snapshot.diagnostics.push(format!("{zero_count}个温度键返回原始0°C；可能为未启用或占位读数，不能据此确认部件实际温度"));
            }
        }
        incomplete |= annotate_sensors(snapshot);
        let any_valid = snapshot.fans.iter().any(|fan| fan.rpm.is_some())
            || snapshot
                .sensors
                .iter()
                .any(|sensor| sensor.temperature_c.is_some());
        let (status, message) = if !any_valid {
            ("unavailable", "当前未取得有效实时读数，请查看诊断")
        } else if incomplete {
            ("partial", "部分读数暂不可用；有效读数来自本次采样")
        } else if matches!(fan_count, Ok(0)) {
            ("no_fans", "FNum 已确认本机没有风扇；温度监测保持只读")
        } else {
            ("ready", "实时只读监测；风扇由系统或现有控制器管理")
        };
        snapshot.status = status.into();
        snapshot.message = message.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Fake {
        values: HashMap<String, f64>,
        index_reads: Cell<u32>,
    }
    impl Fake {
        fn new(values: &[(&str, f64)]) -> Self {
            Self {
                values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                index_reads: Cell::new(0),
            }
        }
    }
    impl Reader for Fake {
        fn number(&self, key: &str, _: &mut HashMap<String, KeyInfo>) -> Result<f64, String> {
            self.values
                .get(key)
                .copied()
                .ok_or_else(|| "模拟读取失败".into())
        }
        fn info(&self, key: &str, _: &mut HashMap<String, KeyInfo>) -> Result<KeyInfo, String> {
            if !self.values.contains_key(key) {
                return Err("模拟键不存在".into());
            }
            let (size, kind) = if key == "FS! " {
                (2, "ui16")
            } else {
                (1, "ui8 ")
            };
            let mut info = KeyInfo::default();
            info.size = size;
            info.kind = smc::fourcc(kind).unwrap();
            Ok(info)
        }
        fn key_at(&self, _: u32) -> Result<String, String> {
            self.index_reads.set(self.index_reads.get() + 1);
            Ok("Xfoo".into())
        }
    }
    #[test]
    fn fan_metadata_preserves_enumerated_channels_and_hardware_readings() {
        let mut snapshot = Snapshot::empty();
        snapshot.model = "Mac16,8".into();
        Monitor::default().collect(
            &Fake::new(&[
                ("FNum", 3.0),
                ("#KEY", 0.0),
                ("F0Ac", 2100.0),
                ("F0Mn", 1200.0),
                ("F0Mx", 6000.0),
                ("F0Tg", 2200.0),
                ("F0Md", 1.0),
                ("F1Ac", 2300.0),
            ]),
            &mut snapshot,
        );
        assert_eq!(snapshot.fan_count, Some(3));
        assert_eq!(snapshot.fans.len(), 3);
        assert_eq!(
            snapshot
                .fans
                .iter()
                .map(|fan| (fan.id.as_str(), fan.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("F0", "左侧"), ("F1", "右侧"), ("F2", "风扇 3")]
        );
        let fan = &snapshot.fans[0];
        assert_eq!(
            (fan.rpm, fan.min_rpm, fan.max_rpm, fan.target_rpm),
            (Some(2100.0), Some(1200.0), Some(6000.0), Some(2200.0))
        );
        assert_eq!(fan.mode, "manual");
        assert_eq!(fan.mode_source, "F0Md");
        assert_eq!(fan.mode_raw, Some(1));
        assert_eq!(fan.evidence[0].line, 7);
        assert_eq!(fan.evidence[1].line, 36);
        assert!(snapshot.fans[2].evidence.is_empty());
    }

    #[test]
    fn fan_catalogue_never_infers_missing_or_dual_channel_counts() {
        for model in ["Mac16,8", "Mac13,1", "iMacPro1,1"] {
            for count in [None, Some(0), Some(1)] {
                let mut snapshot = Snapshot::empty();
                snapshot.model = model.into();
                let mut values = vec![("#KEY", 0.0)];
                if let Some(count) = count {
                    values.push(("FNum", count as f64));
                }
                Monitor::default().collect(&Fake::new(&values), &mut snapshot);
                assert_eq!(snapshot.fan_count, count);
                assert_eq!(snapshot.fans.len(), count.unwrap_or(0) as usize);
            }
        }
        let mut snapshot = Snapshot::empty();
        snapshot.model = "unknown".into();
        Monitor::default().collect(&Fake::new(&[("FNum", 1.0), ("#KEY", 0.0)]), &mut snapshot);
        assert_eq!(snapshot.fans[0].name, "风扇 1");
        assert!(snapshot.fans[0].evidence.is_empty());
    }

    #[test]
    fn fan_metadata_is_optional_on_older_wire_snapshots() {
        let fan: Fan = serde_json::from_value(serde_json::json!({
            "id": "F0", "name": "风扇 1", "rpm": 2100.0,
            "minRpm": null, "maxRpm": null, "targetRpm": null,
            "mode": "unknown", "quality": "valid"
        }))
        .unwrap();
        assert!(fan.evidence.is_empty());
        assert_eq!(fan.note, None);
        let wire = serde_json::to_value(fan).unwrap();
        assert!(wire.get("evidence").is_none());
        assert!(wire.get("note").is_none());
    }

    #[test]
    fn confirmed_zero_is_distinct_from_failed_count() {
        let mut monitor = Monitor::default();
        let mut snapshot = Snapshot::empty();
        monitor.collect(&Fake::new(&[("FNum", 0.0), ("#KEY", 0.0)]), &mut snapshot);
        assert_eq!(snapshot.fan_count, Some(0));
        assert_eq!(snapshot.status, "unavailable");
        let mut snapshot = Snapshot::empty();
        monitor.collect(&Fake::new(&[]), &mut snapshot);
        assert_eq!(snapshot.status, "unavailable");
        assert_eq!(snapshot.fan_count, None);
        assert!(snapshot.fans.is_empty());
    }
    #[test]
    fn later_failure_does_not_reuse_previous_rpm() {
        let mut monitor = Monitor::default();
        let mut snapshot = Snapshot::empty();
        monitor.collect(
            &Fake::new(&[("FNum", 1.0), ("#KEY", 0.0), ("F0Ac", 2100.0)]),
            &mut snapshot,
        );
        assert_eq!(snapshot.fans[0].rpm, Some(2100.0));
        let mut snapshot = Snapshot::empty();
        monitor.collect(&Fake::new(&[("FNum", 1.0)]), &mut snapshot);
        assert_eq!(snapshot.fans[0].rpm, None);
        assert_eq!(snapshot.fans[0].quality, "unavailable");
    }
    #[test]
    fn caches_directory_and_limits_discovery() {
        let mut monitor = Monitor::default();
        let fake = Fake::new(&[("FNum", 0.0), ("#KEY", 9000.0)]);
        monitor.collect(&fake, &mut Snapshot::empty());
        assert_eq!(fake.index_reads.get(), 4096);
        monitor.collect(&fake, &mut Snapshot::empty());
        assert_eq!(fake.index_reads.get(), 4096);
    }

    #[test]
    fn incomplete_directory_is_rediscovered_after_retry_deadline() {
        let fake = Fake::new(&[("#KEY", 2.0)]);
        let mut monitor = Monitor {
            temperature_keys: Some(Vec::new()),
            directory_had_failures: true,
            directory_scanned_at: Some(Instant::now() - Duration::from_secs(61)),
            ..Monitor::default()
        };
        monitor.discover(&fake).unwrap();
        assert_eq!(fake.index_reads.get(), 2);
        assert!(!monitor.directory_had_failures);
        monitor.directory_scanned_at = Some(Instant::now() - Duration::from_secs(301));
        monitor.discover(&fake).unwrap();
        assert_eq!(fake.index_reads.get(), 4);
    }
    #[test]
    fn malformed_count_never_becomes_fanless() {
        for count in [f64::NAN, f64::INFINITY, -1.0, 0.5, 17.0] {
            let mut snapshot = Snapshot::empty();
            Monitor::default()
                .collect(&Fake::new(&[("FNum", count), ("#KEY", 0.0)]), &mut snapshot);
            assert_ne!(snapshot.status, "no_fans");
        }
    }

    #[test]
    fn failed_and_invalid_temperatures_never_retain_a_live_value() {
        let mut monitor = Monitor {
            temperature_keys: Some(vec!["Test".into()]),
            ..Monitor::default()
        };
        for (input, expected_quality, expected_value) in [
            (Some(42.5), "valid", Some(42.5)),
            (None, "unavailable", None),
            (Some(200.0), "invalid", None),
            (Some(f64::NAN), "invalid", None),
            (Some(0.0), "valid", Some(0.0)),
        ] {
            let mut fake = Fake::new(&[("FNum", 0.0)]);
            if let Some(value) = input {
                fake.values.insert("Test".into(), value);
            }
            let mut snapshot = Snapshot::empty();
            monitor.collect(&fake, &mut snapshot);
            assert_eq!(snapshot.sensors[0].temperature_c, expected_value);
            assert_eq!(snapshot.sensors[0].quality, expected_quality);
        }
    }

    fn raw_sensor(key: &str, value: Option<f64>) -> Sensor {
        Sensor {
            sampled_at_ms: None,
            expires_at_ms: None,
            id: key.into(),
            name: key.into(),
            group: "SMC 原始键".into(),
            temperature_c: value,
            quality: if value.is_some() {
                "valid"
            } else {
                "unavailable"
            }
            .into(),
            source: format!("AppleSMC · {key}"),
            classification: "raw".into(),
            primary: false,
            assessment: "unverified".into(),
            interpretation: String::new(),
            aliases: Vec::new(),
            members: Vec::new(),
            evidence: Vec::new(),
        }
    }

    #[test]
    fn sensor_catalogues_are_selected_by_architecture() {
        let mut s = Snapshot::empty();
        s.architecture = "x86_64".into();
        s.model = "UnlistedIntel1,1".into();
        s.sensors = vec![
            raw_sensor("TB0T", Some(30.0)),
            raw_sensor("Tp01", Some(42.0)),
        ];
        assert!(!annotate_sensors(&mut s));
        assert_eq!(s.sensors[0].classification, "mapped");
        assert!(s.sensors[0].evidence[0].file.ends_with("/smcSensors.xml"));
        assert_eq!(s.sensors[1].classification, "raw");
        s.architecture = "unknown".into();
        s.sensors = vec![raw_sensor("TB0T", Some(30.0))];
        assert!(!annotate_sensors(&mut s));
        assert_eq!(s.sensors[0].classification, "raw");
    }

    #[test]
    fn intel_average_uses_actual_keys_and_rejects_case_ambiguity() {
        for ambiguous in [false, true] {
            let mut s = Snapshot::empty();
            s.architecture = "x86_64".into();
            s.model = "UnlistedIntel1,1".into();
            s.sensors = vec![
                raw_sensor("TC0C", Some(40.0)),
                raw_sensor("TC1c", Some(60.0)),
                raw_sensor("Tp0P", Some(2.0)),
            ];
            if ambiguous {
                s.sensors.push(raw_sensor("TC0c", Some(80.0)));
            }
            annotate_sensors(&mut s);
            let average = s
                .sensors
                .iter()
                .find(|sensor| sensor.id == "avg:cpu")
                .unwrap();
            assert_eq!(
                average.temperature_c,
                if ambiguous { None } else { Some(50.0) }
            );
            assert!(average.members.contains(&"TC0C".into()));
            assert_eq!(average.members.len(), if ambiguous { 3 } else { 2 });
            let power = s.sensors.iter().find(|sensor| sensor.id == "Tp0P").unwrap();
            assert_ne!(power.assessment, "suspect");
        }
    }

    #[test]
    fn model_labels_do_not_fabricate_missing_keys_or_unknown_models() {
        let mut s = Snapshot::empty();
        s.model = "UnlistedMac1,1".into();
        s.sensors = vec![
            raw_sensor("TB0T", Some(30.0)),
            raw_sensor("Tp01", Some(42.0)),
        ];
        assert!(!annotate_sensors(&mut s));
        assert_eq!(s.sensors.len(), 2);
        assert_eq!(s.sensors[0].classification, "mapped");
        assert_eq!(s.sensors[1].classification, "raw");
        let mut s = Snapshot::empty();
        s.model = "Mac16,8".into();
        s.sensors = vec![raw_sensor("TB0T", Some(30.0))];
        assert!(annotate_sensors(&mut s));
        assert_eq!(s.sensors[0].name, "电池");
        assert!(s.sensors[0].primary);
        assert!(!s.sensors.iter().any(|x| x.id == "Tp01"));
        assert!(s
            .sensors
            .iter()
            .filter(|x| x.classification == "derived")
            .all(|x| x.temperature_c.is_none()));
    }

    #[test]
    fn aggregate_requires_all_members_and_preserves_suspicious_originals() {
        let model = "Mac16,8";
        let cpu = catalog::catalogue(model)
            .into_iter()
            .find(|d| d.id == "avg:cpu")
            .unwrap();
        for scenario in ["complete", "missing", "failure", "low", "zero"] {
            let mut s = Snapshot::empty();
            s.model = model.into();
            s.sensors = cpu
                .member_ids
                .iter()
                .map(|key| raw_sensor(key, Some(48.0)))
                .collect();
            match scenario {
                "missing" => {
                    s.sensors.remove(0);
                }
                "failure" => {
                    s.sensors[0].quality = "unavailable".into();
                    s.sensors[0].temperature_c = None;
                }
                "low" => {
                    s.sensors[0].temperature_c = Some(2.2);
                }
                "zero" => {
                    s.sensors[0].temperature_c = Some(0.0);
                }
                _ => {}
            }
            annotate_sensors(&mut s);
            let aggregate = s.sensors.iter().find(|x| x.id == "avg:cpu").unwrap();
            assert_eq!(
                aggregate.temperature_c,
                if scenario == "complete" {
                    Some(48.0)
                } else {
                    None
                },
                "{scenario}"
            );
            if matches!(scenario, "low" | "zero") {
                assert_eq!(s.sensors[0].assessment, "suspect");
                assert_eq!(
                    s.sensors[0].temperature_c,
                    Some(if scenario == "low" { 2.2 } else { 0.0 })
                );
            }
        }
    }

    #[test]
    fn mode_protocol_prefers_mask_and_never_calls_unknown_value_automatic() {
        let mut m = Monitor::default();
        let conflict = Fake::new(&[("FS! ", 1.0), ("F0Md", 0.0)]);
        assert_eq!(
            m.mode(&conflict, 0).unwrap(),
            ("manual".into(), "FS! ".into(), 1)
        );
        assert_eq!(m.mode(&conflict, 1).unwrap().0, "automatic");
        for (raw, expected) in [
            (0.0, "automatic"),
            (1.0, "manual"),
            (3.0, "unknown"),
            (2.0, "unknown"),
        ] {
            assert_eq!(m.mode(&Fake::new(&[("F0Md", raw)]), 0).unwrap().0, expected);
        }
        assert_eq!(m.mode(&Fake::new(&[("F0md", 1.0)]), 0).unwrap().1, "F0md");
        assert!(m
            .mode(&Fake::new(&[("FS! ", f64::NAN), ("F0Md", 0.0)]), 0)
            .is_err());
    }

    #[test]
    fn conflicting_direct_mode_keys_are_not_presented_as_automatic() {
        let fake = Fake::new(&[("F0Md", 0.0), ("F0md", 1.0)]);
        assert_eq!(Monitor::default().mode(&fake, 0).unwrap().0, "unknown");
    }

    #[test]
    fn overview_preserves_valid_members_when_no_usable_group_average_exists() {
        for (model, keys) in [
            ("MacBookPro17,2", vec!["Tg0f", "Tg0n"]),
            ("Mac16,8", vec!["Te05", "Te0S", "Tg05"]),
        ] {
            let mut s = Snapshot::empty();
            s.model = model.into();
            s.sensors = keys.iter().map(|key| raw_sensor(key, Some(48.0))).collect();
            annotate_sensors(&mut s);
            for key in keys {
                assert!(
                    s.sensors.iter().find(|x| x.id == key).unwrap().primary,
                    "{model}/{key} hidden"
                );
            }
        }
    }

    #[test]
    fn maximum_directory_with_extreme_diagnostics_fits_bounded_wire() {
        let mut s = Snapshot::empty();
        s.model = "Mac16,8".into();
        for n in 0..4096 {
            let key = format!("T{n:03X}");
            s.sensors.push(raw_sensor(&key, None));
            // AppleSMC decode accepts at most float32; HID below returns float64.
            s.diagnostics.push(format!(
                "{key}：温度 {}°C 超出显示范围 -40…150°C",
                -f64::from(f32::MAX)
            ));
        }
        s.fans = (0..16)
            .map(|n| Fan {
                id: format!("F{n:X}"),
                name: format!("风扇 {}", n + 1),
                evidence: Vec::new(),
                note: None,
                rpm: Some(30000.0),
                min_rpm: Some(0.0),
                max_rpm: Some(30000.0),
                target_rpm: Some(30000.0),
                mode: "unknown".into(),
                mode_source: "F0Md=0, F0md=3".into(),
                mode_raw: Some(3),
                quality: "valid".into(),
            })
            .collect();
        annotate_sensors(&mut s);
        let readings = (1..=512)
            .map(|registry_id| {
                let mut reading = hid_reading(
                    registry_id,
                    &"\0".repeat(256),
                    None,
                    hid::Quality::Unavailable,
                );
                reading.message = Some(format!("原始温度{}°C超出显示范围-40…150°C", -f64::MAX));
                reading
            })
            .collect();
        let mut batch = hid_batch(readings);
        // Control characters expand to six JSON bytes; do not assume UTF-8 width.
        batch.diagnostics = vec!["\0".repeat(512); 64];
        merge_hid(&mut s, batch);
        let mut batch = gpu_batch(
            (1..=64)
                .map(|id| {
                    let mut reading = gpu_reading(id, None, gpu::Quality::Unavailable);
                    reading.name = Some("\0".repeat(128));
                    reading.message = Some("\0".repeat(256));
                    reading
                })
                .collect(),
        );
        batch.diagnostics = vec!["\0".repeat(256); 32];
        merge_gpu(&mut s, batch);
        let mut batch = disk_batch(
            (1..=16)
                .map(|id| {
                    let mut reading = disk_reading(id, None, disk::Quality::Unavailable);
                    reading.name = Some("\0".repeat(128));
                    reading.message = Some("\0".repeat(256));
                    reading
                })
                .collect(),
        );
        batch.diagnostics = vec!["\0".repeat(256); 32];
        merge_disk(&mut s, batch, true, 1001);
        let bytes =
            serde_json::to_vec(&serde_json::json!({"request_id":u64::MAX,"snapshot":s})).unwrap();
        assert!(
            bytes.len() < crate::sampler::MAX_LINE,
            "{} bytes exceeds wire limit",
            bytes.len()
        );
    }
    fn hid_batch(readings: Vec<hid::Reading>) -> hid::Batch {
        hid::Batch {
            available: true,
            captured_at_ms: 100,
            service_count: readings.len(),
            truncated: false,
            readings,
            diagnostics: Vec::new(),
        }
    }

    fn hid_reading(
        registry_id: u64,
        product: &str,
        temperature_c: Option<f64>,
        quality: hid::Quality,
    ) -> hid::Reading {
        hid::Reading {
            source_id: format!("hid:registry:{registry_id:016x}"),
            registry_id,
            product: Some(product.into()),
            temperature_c,
            quality,
            message: None,
        }
    }

    #[test]
    fn hid_remains_visible_when_smc_is_unavailable() {
        let mut snapshot = Snapshot::empty();
        merge_hid(
            &mut snapshot,
            hid_batch(vec![hid_reading(
                1,
                "gas gauge battery",
                Some(32.0),
                hid::Quality::Valid,
            )]),
        );
        assert_eq!(snapshot.status, "partial");
        assert_eq!(snapshot.sensors[0].name, "电池电量计");
        assert_eq!(snapshot.sensors[0].temperature_c, Some(32.0));
        assert!(snapshot.fans.is_empty());
        assert_eq!(snapshot.fan_count, None);
    }

    #[test]
    fn hid_averages_only_discovered_sources_and_never_mix_with_smc() {
        let mut snapshot = Snapshot::empty();
        snapshot.sensors.push(raw_sensor("Te05", Some(90.0)));
        merge_hid(
            &mut snapshot,
            hid_batch(vec![hid_reading(
                1,
                "eACC MTR Temp Sensor0",
                Some(40.0),
                hid::Quality::Valid,
            )]),
        );
        let average = snapshot
            .sensors
            .iter()
            .find(|sensor| sensor.id == "hid:avg:efficiency")
            .unwrap();
        assert_eq!(average.temperature_c, Some(40.0));
        assert_eq!(average.members, vec!["hid:registry:0000000000000001"]);
        assert!(average.interpretation.contains("1/1"));
        assert_eq!(snapshot.sensors.len(), 3);
    }

    #[test]
    fn duplicate_hid_products_are_kept_but_never_silently_averaged() {
        let mut snapshot = Snapshot::empty();
        merge_hid(
            &mut snapshot,
            hid_batch(vec![
                hid_reading(1, "eACC MTR Temp Sensor0", Some(30.0), hid::Quality::Valid),
                hid_reading(2, "eACC MTR Temp Sensor0", Some(90.0), hid::Quality::Valid),
            ]),
        );
        assert_eq!(snapshot.sensors.len(), 3);
        assert_ne!(snapshot.sensors[0].id, snapshot.sensors[1].id);
        assert_ne!(snapshot.sensors[0].name, snapshot.sensors[1].name);
        let average = snapshot
            .sensors
            .iter()
            .find(|sensor| sensor.id == "hid:avg:efficiency")
            .unwrap();
        assert_eq!(average.temperature_c, None);
        assert!(average.interpretation.contains("歧义"));
    }

    #[test]
    fn hid_failed_event_and_suspect_zero_do_not_become_valid_group_temperature() {
        for (temperature, quality) in [
            (None, hid::Quality::Unavailable),
            (Some(0.0), hid::Quality::Suspect),
        ] {
            let mut snapshot = Snapshot::empty();
            merge_hid(
                &mut snapshot,
                hid_batch(vec![hid_reading(
                    1,
                    "eACC MTR Temp Sensor0",
                    temperature,
                    quality,
                )]),
            );
            assert_eq!(snapshot.sensors[0].temperature_c, temperature);
            assert_eq!(snapshot.sensors[1].temperature_c, None);
            assert_eq!(snapshot.sensors[1].quality, "unavailable");
            assert_ne!(snapshot.status, "ready");
        }
    }

    #[test]
    fn hid_failure_preserves_live_smc_and_marks_provider_unavailable() {
        let mut snapshot = Snapshot::empty();
        snapshot.status = "no_fans".into();
        snapshot.fan_count = Some(0);
        snapshot.sensors.push(raw_sensor("TB0T", Some(32.0)));
        let mut batch = hid_batch(Vec::new());
        batch.available = false;
        batch.diagnostics.push("HID provider denied".into());
        merge_hid(&mut snapshot, batch);
        assert_eq!(snapshot.status, "partial");
        assert_eq!(snapshot.fan_count, Some(0));
        assert_eq!(snapshot.sensors[0].temperature_c, Some(32.0));
        assert_eq!(snapshot.providers[0].status, "unavailable");
        assert_eq!(snapshot.diagnostics.last().unwrap(), "HID provider denied");
    }

    fn gpu_batch(readings: Vec<gpu::Reading>) -> gpu::Batch {
        gpu::Batch {
            supported: true,
            available: true,
            captured_at_ms: 100,
            service_count: readings.len(),
            truncated: false,
            readings,
            diagnostics: vec![],
        }
    }

    fn gpu_reading(id: u64, temperature_c: Option<f64>, quality: gpu::Quality) -> gpu::Reading {
        gpu::Reading {
            source_id: format!("gpu:registry:{id:016x}"),
            registry_id: id,
            name: Some("GPU".into()),
            temperature_c,
            quality,
            message: None,
        }
    }

    #[test]
    fn gpu_is_independent_and_never_admits_control_or_hides_smc() {
        let mut snapshot = Snapshot::empty();
        snapshot.sensors.push(raw_sensor("TG0D", Some(35.0)));
        merge_gpu(
            &mut snapshot,
            gpu_batch(vec![gpu_reading(1, Some(40.0), gpu::Quality::Valid)]),
        );
        assert_eq!(snapshot.status, "partial");
        assert_eq!(snapshot.sensors.len(), 2);
        assert_eq!(snapshot.sensors[0].temperature_c, Some(35.0));
        let gpu = &snapshot.sensors[1];
        assert_eq!(gpu.name, "GPU");
        assert_eq!(gpu.group, "GPU");
        assert_eq!(gpu.id, "gpu:registry:0000000000000001");
        assert_eq!(gpu.assessment, "unverified");
        assert!(gpu.primary);
        assert!(gpu.members.is_empty());
        assert_eq!(snapshot.providers[0].decoded, 1);
        assert!(!snapshot.control_available);
        assert_eq!(snapshot.fan_count, None);
    }

    #[test]
    fn gpu_not_applicable_is_neutral_and_cannot_introduce_arm_zero() {
        let mut snapshot = Snapshot::empty();
        snapshot.status = "ready".into();
        let mut batch = gpu_batch(vec![gpu_reading(1, Some(0.0), gpu::Quality::Suspect)]);
        batch.supported = false;
        batch.available = false;
        merge_gpu(&mut snapshot, batch);
        assert_eq!(snapshot.status, "ready");
        assert!(snapshot.sensors.is_empty());
        assert_eq!(snapshot.providers[0].status, "not_applicable");
        assert_eq!(snapshot.providers[0].decoded, 0);
    }

    #[test]
    fn gpu_missing_or_bad_sample_never_reuses_previous_value() {
        let mut first = Snapshot::empty();
        merge_gpu(
            &mut first,
            gpu_batch(vec![gpu_reading(1, Some(50.0), gpu::Quality::Valid)]),
        );
        for quality in [gpu::Quality::Unavailable, gpu::Quality::Invalid] {
            let mut current = Snapshot::empty();
            current.status = "ready".into();
            current.sensors.push(raw_sensor("TB0T", Some(25.0)));
            merge_gpu(&mut current, gpu_batch(vec![gpu_reading(1, None, quality)]));
            assert_eq!(current.status, "partial");
            assert_eq!(current.sensors[1].temperature_c, None);
            assert_eq!(current.providers[0].decoded, 0);
        }
        let mut denied = Snapshot::empty();
        denied.status = "no_fans".into();
        denied.fan_count = Some(0);
        denied.sensors.push(raw_sensor("TB0T", Some(25.0)));
        let mut batch = gpu_batch(vec![]);
        batch.available = false;
        merge_gpu(&mut denied, batch);
        assert_eq!(denied.status, "partial");
        assert_eq!(denied.providers[0].status, "unavailable");
        assert_eq!(denied.fan_count, Some(0));
    }

    #[test]
    fn gpu_zero_duplicates_truncation_and_empty_are_explicit() {
        let mut snapshot = Snapshot::empty();
        merge_gpu(
            &mut snapshot,
            gpu_batch(vec![
                gpu_reading(1, Some(0.0), gpu::Quality::Suspect),
                gpu_reading(2, Some(40.0), gpu::Quality::Valid),
            ]),
        );
        assert_eq!(snapshot.sensors[0].assessment, "suspect");
        assert_eq!(snapshot.sensors[0].temperature_c, Some(0.0));
        assert_ne!(snapshot.sensors[0].name, snapshot.sensors[1].name);
        assert_eq!(snapshot.providers[0].status, "partial");
        let mut empty = Snapshot::empty();
        let mut batch = gpu_batch(vec![]);
        batch.service_count = 2;
        merge_gpu(&mut empty, batch);
        assert_eq!(empty.providers[0].status, "empty");
        let mut limited = Snapshot::empty();
        let mut batch = gpu_batch(vec![gpu_reading(1, Some(42.0), gpu::Quality::Valid)]);
        batch.truncated = true;
        merge_gpu(&mut limited, batch);
        assert_eq!(limited.providers[0].status, "partial");
        assert_eq!(limited.status, "partial");
    }

    fn disk_batch(readings: Vec<disk::Reading>) -> disk::Batch {
        disk::Batch {
            supported: true,
            available: true,
            captured_at_ms: 1000,
            service_count: readings.len(),
            truncated: false,
            readings,
            diagnostics: vec![],
        }
    }

    fn disk_reading(id: u64, temperature_c: Option<f64>, quality: disk::Quality) -> disk::Reading {
        disk::Reading {
            source_id: format!("disk:nvme:registry:{id:016x}"),
            registry_id: id,
            name: Some("SSD".into()),
            transport: "nvme".into(),
            temperature_c,
            quality,
            message: None,
        }
    }

    #[test]
    fn disk_cache_preserves_timestamps_and_independent_identity() {
        let mut snapshot = Snapshot::empty();
        snapshot.captured_at_ms = 20000;
        snapshot.sensors.push(raw_sensor("TH0P", Some(28.0)));
        merge_disk(
            &mut snapshot,
            disk_batch(vec![disk_reading(1, Some(25.85), disk::Quality::Valid)]),
            true,
            20000,
        );
        let sensor = &snapshot.sensors[1];
        assert_eq!(sensor.sampled_at_ms, Some(1000));
        assert_eq!(sensor.expires_at_ms, Some(46000));
        assert_eq!(snapshot.captured_at_ms, 20000);
        assert_eq!(sensor.temperature_c, Some(25.85));
        assert_eq!(snapshot.sensors[0].temperature_c, Some(28.0));
        assert!(sensor.id.starts_with("disk:nvme:registry:"));
        assert_eq!(sensor.assessment, "unverified");
        assert_eq!(snapshot.providers[0].captured_at_ms, 1000);
        assert!(snapshot.providers[0].cached);
        assert_eq!(snapshot.providers[0].decoded, 1);
        assert_eq!(snapshot.status, "partial");
        assert!(!snapshot.control_available);
        assert_eq!(snapshot.fan_count, None);
    }

    #[test]
    fn disk_expiry_future_and_overflow_never_return_live_values() {
        for (sampled, observed, valid) in [
            (1000, 1000, true),
            (1000, 45999, true),
            (1000, 46000, false),
            (1000, 999, false),
            (u64::MAX - 1, u64::MAX, false),
        ] {
            let mut snapshot = Snapshot::empty();
            snapshot.sensors.push(raw_sensor("TB0T", Some(24.0)));
            let mut batch = disk_batch(vec![disk_reading(1, Some(25.85), disk::Quality::Valid)]);
            batch.captured_at_ms = sampled;
            merge_disk(&mut snapshot, batch, true, observed);
            let disk = &snapshot.sensors[1];
            assert_eq!(disk.temperature_c, valid.then_some(25.85));
            assert_eq!(disk.quality, if valid { "valid" } else { "stale" });
            assert_eq!(snapshot.providers[0].decoded, usize::from(valid));
            assert_eq!(disk.sampled_at_ms, Some(sampled));
        }
    }

    #[test]
    fn disk_failures_duplicates_limits_and_empty_preserve_other_providers() {
        let mut snapshot = Snapshot::empty();
        snapshot.status = "no_fans".into();
        snapshot.fan_count = Some(0);
        snapshot.sensors.push(raw_sensor("TB0T", Some(24.0)));
        let mut batch = disk_batch(vec![
            disk_reading(1, None, disk::Quality::Unavailable),
            disk_reading(2, None, disk::Quality::Invalid),
            disk_reading(3, None, disk::Quality::Suspect),
        ]);
        batch.truncated = true;
        merge_disk(&mut snapshot, batch, false, 1000);
        assert_eq!(snapshot.status, "partial");
        assert_eq!(snapshot.fan_count, Some(0));
        assert_eq!(snapshot.sensors[0].temperature_c, Some(24.0));
        assert!(snapshot.sensors[1..]
            .iter()
            .all(|s| s.temperature_c.is_none()));
        assert_ne!(snapshot.sensors[1].name, snapshot.sensors[2].name);
        assert_eq!(snapshot.providers[0].status, "partial");
        assert_eq!(snapshot.providers[0].decoded, 0);
        assert_eq!(snapshot.sensors[3].quality, "invalid");
        let mut empty = Snapshot::empty();
        let mut batch = disk_batch(vec![]);
        batch.service_count = 2;
        merge_disk(&mut empty, batch, false, 1000);
        assert_eq!(empty.providers[0].status, "empty");
        let mut denied = Snapshot::empty();
        let mut batch = disk_batch(vec![]);
        batch.available = false;
        merge_disk(&mut denied, batch, true, 1000);
        assert_eq!(denied.providers[0].status, "unavailable");
        let mut unsupported = Snapshot::empty();
        unsupported.status = "ready".into();
        let mut batch = disk_batch(vec![disk_reading(1, Some(20.0), disk::Quality::Valid)]);
        batch.supported = false;
        merge_disk(&mut unsupported, batch, false, 1000);
        assert!(unsupported.sensors.is_empty());
        assert_eq!(unsupported.status, "ready");
        assert_eq!(unsupported.providers[0].status, "not_applicable");
    }

    #[test]
    fn sensor_timing_is_optional_for_legacy_sources_and_preserved_in_wire() {
        let legacy = raw_sensor("TB0T", Some(24.0));
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert!(encoded.get("sampledAtMs").is_none());
        assert!(encoded.get("expiresAtMs").is_none());
        let decoded: Sensor = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.sampled_at_ms, None);
        let mut snapshot = Snapshot::empty();
        merge_disk(
            &mut snapshot,
            disk_batch(vec![disk_reading(1, Some(26.85), disk::Quality::Valid)]),
            true,
            1100,
        );
        let encoded = serde_json::to_value(&snapshot.sensors[0]).unwrap();
        assert_eq!(encoded["sampledAtMs"], 1000);
        assert_eq!(encoded["expiresAtMs"], 46000);
        let decoded: Sensor = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.temperature_c, Some(26.85));
    }
}
