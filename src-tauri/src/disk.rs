//! SDK-backed read-only SMART sampler. Call only in the existing isolated worker.
//! Registry identity lasts for the service lifetime, not across boots. No caching here.
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Valid,
    Suspect,
    Invalid,
    Unavailable,
}
impl Quality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Suspect => "suspect",
            Self::Invalid => "invalid",
            Self::Unavailable => "unavailable",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Reading {
    pub source_id: String,
    pub registry_id: u64,
    pub name: Option<String>,
    pub transport: String,
    pub temperature_c: Option<f64>,
    pub quality: Quality,
    pub message: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Batch {
    pub supported: bool,
    pub available: bool,
    pub captured_at_ms: u64,
    pub service_count: usize,
    pub truncated: bool,
    pub readings: Vec<Reading>,
    pub diagnostics: Vec<String>,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NativeReading {
    registry_id: u64,
    transport: u32,
    stage: u32,
    status: i32,
    validation_status: i32,
    raw_valid: u32,
    reserved: u32,
    model: [u8; 128],
    raw: [u8; 512],
}
#[repr(C)]
struct NativeBatch {
    abi_version: u32,
    size: u32,
    reading_size: u32,
    supported: u32,
    service_count: u32,
    reading_count: u32,
    truncated: u32,
    enumeration_status: i32,
    readings: [NativeReading; 16],
}
const _: () = {
    assert!(std::mem::size_of::<NativeReading>() == 672);
    assert!(std::mem::offset_of!(NativeReading, model) == 32);
    assert!(std::mem::offset_of!(NativeReading, raw) == 160);
    assert!(std::mem::size_of::<NativeBatch>() == 10784);
    assert!(std::mem::offset_of!(NativeBatch, readings) == 32);
};
#[cfg(target_os = "macos")]
extern "C" {
    fn mf_disk_sample(output: *mut NativeBatch, capacity: u32) -> i32;
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn empty(supported: bool, captured_at_ms: u64) -> Batch {
    Batch {
        supported,
        available: false,
        captured_at_ms,
        service_count: 0,
        truncated: false,
        readings: Vec::new(),
        diagnostics: Vec::new(),
    }
}
fn diagnostic(batch: &mut Batch, message: String) {
    if batch.diagnostics.len() < 32 {
        batch.diagnostics.push(message.chars().take(256).collect());
    }
}
fn result(
    value: Option<f64>,
    quality: Quality,
    message: &str,
) -> (Option<f64>, Quality, Option<String>) {
    (
        value,
        quality,
        if message.is_empty() {
            None
        } else {
            Some(message.into())
        },
    )
}
fn checked_temperature(value: f64) -> (Option<f64>, Quality, Option<String>) {
    if !value.is_finite() || !(-40.0..=150.0).contains(&value) {
        result(
            None,
            Quality::Invalid,
            "Temperature outside supported range",
        )
    } else {
        result(Some(value), Quality::Valid, "")
    }
}
fn parse(raw: &[u8; 512], transport: u32) -> (Option<f64>, Quality, Option<String>) {
    if transport == 2 {
        let kelvin = u16::from_le_bytes([raw[1], raw[2]]);
        return if kelvin == 0 {
            result(
                None,
                Quality::Unavailable,
                "Composite temperature unavailable",
            )
        } else {
            checked_temperature(f64::from(kelvin) - 273.15)
        };
    }
    if raw.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) != 0 {
        return result(None, Quality::Invalid, "ATA checksum mismatch");
    }
    // Attribute 194 has priority even when zero or conflicting. Never guess between duplicates.
    for id in [194u8, 190u8] {
        let mut value = None;
        for i in 0..30 {
            if raw[2 + 12 * i] == id {
                let next = raw[7 + 12 * i];
                if value.is_some_and(|previous| previous != next) {
                    return result(
                        None,
                        Quality::Suspect,
                        "Conflicting duplicate ATA temperature attributes",
                    );
                }
                value = Some(next);
            }
        }
        if let Some(value) = value {
            return if value == 0 {
                result(None, Quality::Unavailable, "ATA temperature unavailable")
            } else {
                checked_temperature(f64::from(value))
            };
        }
    }
    result(
        None,
        Quality::Unavailable,
        "No supported ATA temperature attribute",
    )
}
fn decode(native: &NativeBatch, status: i32, captured_at_ms: u64) -> Batch {
    let mut batch = empty(true, captured_at_ms);
    if status != 0
        || native.abi_version != 1
        || native.size != 10784
        || native.reading_size != 672
        || native.reading_count > 16
        || native.service_count > 256
        || native.supported != 1
        || native.truncated > 1
    {
        diagnostic(
            &mut batch,
            format!("Disk native ABI/call failure: {status:#010x}"),
        );
        return batch;
    }
    batch.available = native.enumeration_status == 0;
    batch.service_count = native.service_count as usize;
    batch.truncated = native.truncated != 0;
    if native.enumeration_status != 0 {
        diagnostic(
            &mut batch,
            format!(
                "Disk enumeration incomplete: {:#010x}",
                native.enumeration_status
            ),
        );
    }
    if batch.truncated {
        diagnostic(
            &mut batch,
            "Disk enumeration truncated or identity unavailable".into(),
        );
    }
    let mut seen = HashSet::new();
    for record in &native.readings[..native.reading_count as usize] {
        if record.registry_id == 0 || !(1..=2).contains(&record.transport) {
            batch.truncated = true;
            diagnostic(
                &mut batch,
                "Disk record omitted: invalid identity/transport".into(),
            );
            continue;
        }
        if !seen.insert(record.registry_id) {
            continue;
        }
        let transport = if record.transport == 2 { "nvme" } else { "ata" };
        let (temperature_c, quality, message) = if record.raw_valid == 1
            && record.stage == 5
            && record.status == 0
            && (record.transport == 2 || record.validation_status == 0)
        {
            parse(&record.raw, record.transport)
        } else {
            result(
                None,
                if record.stage == 4 {
                    Quality::Invalid
                } else {
                    Quality::Unavailable
                },
                &format!(
                    "SMART stage {} failed: status={:#010x}, validation={:#010x}",
                    record.stage, record.status, record.validation_status
                ),
            )
        };
        let end = record
            .model
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(record.model.len());
        let name = std::str::from_utf8(&record.model[..end])
            .ok()
            .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
            .filter(|s| !s.trim().is_empty());
        if let Some(ref message) = message {
            diagnostic(
                &mut batch,
                format!("{transport}:{:016x}: {message}", record.registry_id),
            );
        }
        batch.readings.push(Reading {
            source_id: format!("disk:{transport}:registry:{:016x}", record.registry_id),
            registry_id: record.registry_id,
            name,
            transport: transport.into(),
            temperature_c,
            quality,
            message,
        });
    }
    batch
}
// Capture before entering synchronous native I/O, including all failure paths.
fn sample_native(read: impl FnOnce(&mut NativeBatch) -> i32, clock: impl FnOnce() -> u64) -> Batch {
    let captured_at_ms = clock();
    let mut native: NativeBatch = unsafe { std::mem::zeroed() };
    let status = read(&mut native);
    decode(&native, status, captured_at_ms)
}
pub fn sample() -> Batch {
    #[cfg(target_os = "macos")]
    {
        sample_native(
            |native| unsafe { mf_disk_sample(native, std::mem::size_of::<NativeBatch>() as u32) },
            now_ms,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut batch = empty(false, now_ms());
        diagnostic(&mut batch, "Disk SMART provider requires macOS".into());
        batch
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn checksum(raw: &mut [u8; 512]) {
        raw[511] = 0;
        raw[511] = 0u8.wrapping_sub(raw.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
    }
    fn native() -> NativeBatch {
        let mut n: NativeBatch = unsafe { std::mem::zeroed() };
        n.abi_version = 1;
        n.size = 10784;
        n.reading_size = 672;
        n.supported = 1;
        n
    }
    #[test]
    fn nvme_fraction_and_zero_and_range() {
        let mut raw = [0; 512];
        assert_eq!(parse(&raw, 2).1, Quality::Unavailable);
        raw[1..3].copy_from_slice(&299u16.to_le_bytes());
        assert!((parse(&raw, 2).0.unwrap() - 25.85).abs() < 0.00001);
        raw[1..3].copy_from_slice(&500u16.to_le_bytes());
        assert_eq!(parse(&raw, 2).1, Quality::Invalid);
    }
    #[test]
    fn ata_priority_zero_checksum_and_conflict() {
        let mut raw = [0; 512];
        raw[2] = 190;
        raw[7] = 30;
        checksum(&mut raw);
        assert_eq!(parse(&raw, 1).0, Some(30.0));
        raw[14] = 194;
        raw[19] = 40;
        checksum(&mut raw);
        assert_eq!(parse(&raw, 1).0, Some(40.0));
        raw[19] = 0;
        checksum(&mut raw);
        assert_eq!(parse(&raw, 1).1, Quality::Unavailable);
        raw[26] = 194;
        raw[31] = 50;
        checksum(&mut raw);
        assert_eq!(parse(&raw, 1).1, Quality::Suspect);
        raw[511] ^= 1;
        assert_eq!(parse(&raw, 1).1, Quality::Invalid);
    }
    #[test]
    fn failure_buffers_are_never_consumed_and_identity_deduplicated() {
        let mut n = native();
        n.reading_count = 3;
        n.readings[0].registry_id = 123;
        n.readings[0].transport = 2;
        n.readings[0].raw[1..3].copy_from_slice(&299u16.to_le_bytes());
        n.readings[0].status = -1;
        n.readings[0].stage = 3;
        n.readings[0].raw_valid = 1;
        n.readings[1] = n.readings[0];
        n.readings[2].transport = 2;
        let b = decode(&n, 0, 123);
        assert_eq!(b.readings.len(), 1);
        assert_eq!(b.readings[0].temperature_c, None);
        assert!(b.truncated);
        assert_eq!(
            b.readings[0].source_id,
            "disk:nvme:registry:000000000000007b"
        );
    }
    #[test]
    fn abi_rejection_and_empty_success() {
        let mut n = native();
        assert!(decode(&n, 0, 123).available);
        n.size -= 1;
        assert!(!decode(&n, 0, 123).available);
        n = native();
        n.reading_count = 17;
        assert!(!decode(&n, 0, 123).available);
        n = native();
        n.enumeration_status = -1;
        assert!(!decode(&n, 0, 123).available);
    }
    #[test]
    fn ata_sdk_validation_required() {
        let mut n = native();
        n.reading_count = 1;
        let r = &mut n.readings[0];
        r.registry_id = 1;
        r.transport = 1;
        r.stage = 4;
        r.validation_status = -1;
        r.raw[2] = 194;
        r.raw[7] = 30;
        checksum(&mut r.raw);
        assert_eq!(decode(&n, 0, 123).readings[0].quality, Quality::Invalid);
    }
    #[test]
    fn native_start_timestamp_survives_slow_io_and_failure() {
        use std::cell::Cell;
        let phase = Cell::new(0);
        let b = sample_native(
            |_| {
                assert_eq!(phase.get(), 1);
                phase.set(2);
                -1
            },
            || {
                phase.set(1);
                123
            },
        );
        assert_eq!(b.captured_at_ms, 123);
        assert_eq!(phase.get(), 2);
        assert!(b.supported);
        assert!(!b.available);
        let b = sample_native(
            |n| {
                *n = native();
                0
            },
            || 456,
        );
        assert_eq!(b.captured_at_ms, 456);
        assert!(b.supported && b.available && b.readings.is_empty());
    }
    #[test]
    fn abi_failure_is_supported_but_unavailable() {
        for fault in 0..4 {
            let mut n = native();
            let status = match fault {
                0 => -1,
                1 => {
                    n.abi_version = 99;
                    0
                }
                2 => {
                    n.supported = 0;
                    0
                }
                _ => {
                    n.size = 0;
                    0
                }
            };
            let b = decode(&n, status, 789);
            assert!(b.supported);
            assert!(!b.available);
            assert_eq!(b.captured_at_ms, 789);
            assert!(!b.diagnostics.is_empty());
        }
    }
}
