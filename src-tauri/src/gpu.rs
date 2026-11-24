//! Read-only Intel registry temperatures. Call only in the isolated sampler process.
//! No cross-sample state, SMC fallback, Metal association, or hardware writes.
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SERVICES: usize = 256;
const MAX_NODES: usize = 2048;
const MAX_DEPTH: usize = 16;
const MAX_READINGS: usize = 64;
const MAX_DIAGNOSTICS: usize = 32;

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
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Registry lifetime identity, not a persistent physical GPU identifier.
    pub source_id: String,
    pub registry_id: u64,
    pub name: Option<String>,
    pub temperature_c: Option<f64>,
    pub quality: Quality,
    pub message: Option<String>,
}
#[derive(Debug)]
pub struct Batch {
    pub supported: bool,
    pub available: bool,
    pub captured_at_ms: u64,
    /// Number of PCI services examined, including non-display devices.
    pub service_count: usize,
    pub truncated: bool,
    pub readings: Vec<Reading>,
    pub diagnostics: Vec<String>,
}
impl Batch {
    fn empty() -> Self {
        Self {
            supported: cfg!(all(target_os = "macos", target_arch = "x86_64")),
            available: false,
            captured_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            service_count: 0,
            truncated: false,
            readings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
    fn diagnostic(&mut self, message: impl Into<String>) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics
                .push(message.into().chars().take(256).collect());
        }
    }
    fn limit(&mut self, message: &str) {
        self.truncated = true;
        self.diagnostic(message);
    }
}
enum Value {
    Number(f64),
    Missing,
    Invalid(&'static str),
}
fn classify(value: Value) -> (Option<f64>, Quality, Option<String>) {
    match value {
        Value::Missing => (
            None,
            Quality::Unavailable,
            Some("Temperature(C) missing; no current reading".into()),
        ),
        Value::Invalid(message) => (None, Quality::Invalid, Some(message.into())),
        Value::Number(v) if !v.is_finite() || !(-40.0..=150.0).contains(&v) => (
            None,
            Quality::Invalid,
            Some("Temperature(C) is non-finite or outside -40..150 C".into()),
        ),
        Value::Number(0.0) => (
            Some(0.0),
            Quality::Suspect,
            Some("Raw 0 C may be an inactive sensor or placeholder".into()),
        ),
        Value::Number(v) => (Some(v), Quality::Valid, None),
    }
}
fn display_class(bytes: &[u8]) -> bool {
    bytes.get(..4).is_some_and(|b| {
        matches!(
            u32::from_le_bytes(b.try_into().unwrap()),
            0x30000 | 0x38000 | 0x30200
        )
    })
}
#[derive(Default)]
struct Collector {
    ids: HashSet<u64>,
}
impl Collector {
    fn push(&mut self, batch: &mut Batch, id: Option<u64>, name: Option<String>, value: Value) {
        let Some(id) = id.filter(|id| *id != 0) else {
            batch.limit("GPU property owner lacks a valid registry ID; omitted");
            return;
        };
        if !self.ids.insert(id) {
            return;
        }
        if batch.readings.len() >= MAX_READINGS {
            batch.limit("GPU reading limit reached");
            return;
        }
        let (temperature_c, quality, message) = classify(value);
        batch.readings.push(Reading {
            source_id: format!("gpu:registry:{id:016x}"),
            registry_id: id,
            name,
            temperature_c,
            quality,
            message,
        });
    }
}

// Compile native declarations on both macOS architectures; only Intel sample() invokes them.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_void, CStr};
    type Cf = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Cf);
        fn CFGetTypeID(value: Cf) -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFDataGetLength(value: Cf) -> isize;
        fn CFDataGetBytePtr(value: Cf) -> *const u8;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFDictionaryGetValue(dict: Cf, key: Cf) -> Cf;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(value: Cf, kind: isize, result: *mut c_void) -> u8;
        fn CFStringCreateWithCString(allocator: Cf, text: *const c_char, encoding: u32) -> Cf;
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingServices(port: u32, matching: Cf, iterator: *mut u32) -> i32;
        fn IOIteratorNext(iterator: u32) -> u32;
        fn IOIteratorIsValid(iterator: u32) -> u32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IORegistryEntryCreateCFProperty(entry: u32, key: Cf, allocator: Cf, options: u32) -> Cf;
        fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
        fn IORegistryEntryGetName(entry: u32, name: *mut c_char) -> i32;
        fn IORegistryEntryGetChildIterator(
            entry: u32,
            plane: *const c_char,
            iterator: *mut u32,
        ) -> i32;
    }
    struct OwnedCf(Cf);
    impl OwnedCf {
        fn new(pointer: Cf) -> Option<Self> {
            // Construct the RAII owner lazily: then_some would Drop a null owner.
            (!pointer.is_null()).then(|| Self(pointer))
        }
        fn key(key: &CStr) -> Option<Self> {
            Self::new(unsafe {
                CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), 0x08000100)
            })
        }
    }
    impl Drop for OwnedCf {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    struct OwnedIo(u32);
    impl OwnedIo {
        fn new(object: u32) -> Option<Self> {
            (object != 0).then(|| Self(object))
        }
    }
    impl Drop for OwnedIo {
        fn drop(&mut self) {
            unsafe {
                IOObjectRelease(self.0);
            }
        }
    }
    struct Keys {
        class: OwnedCf,
        stats: OwnedCf,
        temperature: OwnedCf,
    }
    impl Keys {
        fn new() -> Option<Self> {
            Some(Self {
                class: OwnedCf::key(c"class-code")?,
                stats: OwnedCf::key(c"PerformanceStatistics")?,
                temperature: OwnedCf::key(c"Temperature(C)")?,
            })
        }
    }
    fn property(entry: u32, key: &OwnedCf) -> Option<OwnedCf> {
        OwnedCf::new(unsafe { IORegistryEntryCreateCFProperty(entry, key.0, std::ptr::null(), 0) })
    }
    fn is_display(entry: u32, keys: &Keys) -> bool {
        let Some(data) = property(entry, &keys.class) else {
            return false;
        };
        unsafe {
            if CFGetTypeID(data.0) != CFDataGetTypeID() || CFDataGetLength(data.0) < 4 {
                return false;
            }
            let bytes = CFDataGetBytePtr(data.0);
            !bytes.is_null() && display_class(std::slice::from_raw_parts(bytes, 4))
        }
    }
    fn identity(entry: u32) -> Option<u64> {
        let mut id = 0;
        (unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) } == 0).then_some(id)
    }
    fn name(entry: u32) -> Option<String> {
        let mut bytes = [0u8; 128];
        if unsafe { IORegistryEntryGetName(entry, bytes.as_mut_ptr().cast()) } != 0 {
            return None;
        }
        let end = bytes.iter().position(|b| *b == 0)?;
        (end > 0).then(|| String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
    fn temperature(stats: &OwnedCf, keys: &Keys) -> Value {
        unsafe {
            if CFGetTypeID(stats.0) != CFDictionaryGetTypeID() {
                return Value::Invalid("PerformanceStatistics is not a CFDictionary");
            }
            let number = CFDictionaryGetValue(stats.0, keys.temperature.0);
            if number.is_null() {
                return Value::Missing;
            }
            if CFGetTypeID(number) != CFNumberGetTypeID() {
                return Value::Invalid("Temperature(C) is not a CFNumber");
            }
            let mut result = 0.0f64;
            if CFNumberGetValue(
                number,
                13, /* kCFNumberDoubleType */
                (&mut result as *mut f64).cast(),
            ) == 0
            {
                return Value::Invalid("Cannot convert Temperature(C) to double");
            }
            Value::Number(result)
        }
    }
    struct Scan<'a> {
        keys: &'a Keys,
        collector: Collector,
        nodes: usize,
        batch: &'a mut Batch,
    }
    impl Scan<'_> {
        fn visit(&mut self, node: u32, depth: usize) {
            if self.nodes >= MAX_NODES {
                self.batch.limit("GPU registry node limit reached");
                return;
            }
            self.nodes += 1;
            // Direct property lookup preserves the actual property owner's identity.
            if let Some(stats) = property(node, &self.keys.stats) {
                self.collector.push(
                    self.batch,
                    identity(node),
                    name(node),
                    temperature(&stats, self.keys),
                );
            }
            let mut raw = 0;
            let status =
                unsafe { IORegistryEntryGetChildIterator(node, c"IOService".as_ptr(), &mut raw) };
            let iterator = OwnedIo::new(raw);
            if status != 0 {
                self.batch.truncated = true;
                self.batch
                    .diagnostic(format!("GPU child enumeration failed: {status:#x}"));
                return;
            }
            let Some(iterator) = iterator else {
                self.batch.truncated = true;
                self.batch
                    .diagnostic("GPU child enumeration returned no iterator");
                return;
            };
            while let Some(child) = OwnedIo::new(unsafe { IOIteratorNext(iterator.0) }) {
                if depth >= MAX_DEPTH {
                    self.batch.limit("GPU registry depth limit reached");
                    break;
                }
                if self.nodes >= MAX_NODES {
                    self.batch.limit("GPU registry node limit reached");
                    break;
                }
                self.visit(child.0, depth + 1);
            }
            if unsafe { IOIteratorIsValid(iterator.0) } == 0 {
                self.batch.diagnostic(
                    "GPU registry changed during child enumeration; snapshot incomplete",
                );
                self.batch.truncated = true;
            }
        }
    }
    pub(super) fn sample() -> Batch {
        let mut batch = Batch::empty();
        let Some(keys) = Keys::new() else {
            batch.diagnostic("GPU CF key allocation failed");
            return batch;
        };
        let matching = unsafe { IOServiceMatching(c"IOPCIDevice".as_ptr()) };
        if matching.is_null() {
            batch.diagnostic("GPU service matching allocation failed");
            return batch;
        }
        let mut raw = 0;
        // IOServiceGetMatchingServices consumes matching even on failure.
        let status = unsafe { IOServiceGetMatchingServices(0, matching, &mut raw) };
        let iterator = OwnedIo::new(raw);
        if status != 0 {
            batch.diagnostic(format!("GPU service enumeration failed: {status:#x}"));
            return batch;
        }
        // The SDK explicitly permits success + null iterator for no matches.
        batch.available = true;
        let Some(iterator) = iterator else {
            batch.diagnostic("No IOPCIDevice services matched this sample");
            return batch;
        };
        let mut scan = Scan {
            keys: &keys,
            collector: Collector::default(),
            nodes: 0,
            batch: &mut batch,
        };
        while let Some(service) = OwnedIo::new(unsafe { IOIteratorNext(iterator.0) }) {
            if scan.batch.service_count >= MAX_SERVICES {
                scan.batch.limit("GPU PCI service limit reached");
                break;
            }
            scan.batch.service_count += 1;
            if is_display(service.0, &keys) {
                scan.visit(service.0, 0);
            }
        }
        if unsafe { IOIteratorIsValid(iterator.0) } == 0 {
            scan.batch
                .limit("GPU registry changed during PCI enumeration; snapshot incomplete");
        }
        scan.batch
            .readings
            .sort_by_key(|reading| reading.registry_id);
        if scan.batch.readings.is_empty() {
            scan.batch.diagnostic(
                "No display-class registry node provided PerformanceStatistics this sample",
            );
        }
        batch
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn null_handles_never_construct_raii_owners() {
            assert!(OwnedCf::new(std::ptr::null()).is_none());
            assert!(OwnedIo::new(0).is_none());
            let keys = Keys::new().expect("CF keys");
            // Valid owners still release normally at scope exit.
            assert_ne!(unsafe { CFGetTypeID(keys.temperature.0) }, 0);
        }
    }
}
pub fn sample() -> Batch {
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        platform::sample()
    }
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    {
        let mut batch = Batch::empty();
        batch.diagnostic(
            "not_applicable: dedicated registry GPU temperature is supported only on Intel macOS",
        );
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pci_classes_are_little_endian() {
        for code in [0x30000u32, 0x38000, 0x30200] {
            assert!(display_class(&code.to_le_bytes()));
        }
        assert!(!display_class(&[0, 3, 0, 0]));
        assert!(!display_class(&[0, 0, 3]));
    }
    #[test]
    fn validates_without_synthesizing_zero() {
        for v in [f64::NAN, f64::INFINITY, -40.1, 150.1] {
            let (v, q, _) = classify(Value::Number(v));
            assert_eq!((v, q), (None, Quality::Invalid));
        }
        assert_eq!(classify(Value::Missing).0, None);
        assert_eq!(classify(Value::Invalid("failure")).1, Quality::Invalid);
        assert_eq!(classify(Value::Number(0.0)).1, Quality::Suspect);
        for v in [-40.0, 38.5, 150.0] {
            assert_eq!(
                (classify(Value::Number(v)).0, classify(Value::Number(v)).1),
                (Some(v), Quality::Valid)
            );
        }
    }
    #[test]
    fn identity_deduplicates_owner_and_failure_is_fresh() {
        let mut batch = Batch::empty();
        let mut collector = Collector::default();
        collector.push(&mut batch, Some(42), None, Value::Number(65.0));
        collector.push(&mut batch, Some(42), None, Value::Number(66.0));
        assert_eq!(batch.readings.len(), 1);
        assert_eq!(batch.readings[0].source_id, "gpu:registry:000000000000002a");
        let mut fresh = Batch::empty();
        Collector::default().push(&mut fresh, Some(42), None, Value::Missing);
        assert_eq!(fresh.readings[0].temperature_c, None);
        assert_eq!(fresh.readings[0].quality, Quality::Unavailable);
        collector.push(&mut batch, None, None, Value::Number(40.0));
        assert!(batch.truncated);
    }
    #[test]
    fn bounded_output() {
        let mut batch = Batch::empty();
        let mut collector = Collector::default();
        for id in 1..=MAX_READINGS as u64 + 1 {
            collector.push(&mut batch, Some(id), None, Value::Missing);
        }
        assert_eq!(batch.readings.len(), MAX_READINGS);
        assert!(batch.truncated);
        for _ in 0..100 {
            batch.diagnostic("error");
        }
        assert_eq!(batch.diagnostics.len(), MAX_DIAGNOSTICS);
    }
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn unsupported_does_not_enumerate() {
        let batch = sample();
        assert!(!batch.supported);
        assert!(!batch.available);
        assert_eq!(batch.service_count, 0);
        assert!(batch.readings.is_empty());
    }
}
