//! Read-only HID temperature provider, independent of Product/component mapping.
//! Private IOHID symbols are resolved at runtime; a missing symbol cannot break
//! application startup. Call only inside the existing isolated sampler process:
//! synchronous CF/IOHID calls here have no safe in-process hard cancellation.
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SERVICES: usize = 512;
const MAX_PRODUCT_UTF16: usize = 256;
const MAX_DIAGNOSTICS: usize = 64;
const MIN_TEMPERATURE_C: f64 = -40.0;
const MAX_TEMPERATURE_C: f64 = 150.0;

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
    /// Stable for this registry service's lifetime; never derived from Product.
    pub source_id: String,
    pub registry_id: u64,
    pub product: Option<String>,
    pub temperature_c: Option<f64>,
    pub quality: Quality,
    pub message: Option<String>,
}

#[derive(Debug)]
pub struct Batch {
    /// Successfully reached a valid services array, not a claim of valid temperatures.
    pub available: bool,
    pub captured_at_ms: u64,
    pub service_count: usize,
    /// Includes enumeration limits and services omitted for untrusted identities.
    pub truncated: bool,
    pub readings: Vec<Reading>,
    pub diagnostics: Vec<String>,
}
impl Batch {
    fn empty() -> Self {
        Self {
            available: false,
            captured_at_ms: now_ms(),
            service_count: 0,
            truncated: false,
            readings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Default)]
struct Diagnostics {
    entries: Vec<String>,
    suppressed: usize,
}
impl Diagnostics {
    fn push(&mut self, message: impl Into<String>) {
        if self.entries.len() < MAX_DIAGNOSTICS - 1 {
            self.entries
                .push(message.into().chars().take(512).collect());
        } else {
            self.suppressed += 1;
        }
    }
    fn finish(mut self) -> Vec<String> {
        if self.suppressed > 0 {
            self.entries
                .push(format!("另有{}条HID诊断因数量上限被省略", self.suppressed));
        }
        self.entries
    }
}

enum EventResult {
    Value(f64),
    Missing,
    Invalid(&'static str),
}
fn classify(value: EventResult) -> (Option<f64>, Quality, Option<String>) {
    match value {
        EventResult::Missing => (
            None,
            Quality::Unavailable,
            Some("温度事件为空；未取得本次读数".into()),
        ),
        EventResult::Invalid(reason) => (None, Quality::Invalid, Some(reason.into())),
        EventResult::Value(value) if !value.is_finite() => {
            (None, Quality::Invalid, Some("温度为NaN或无穷值".into()))
        }
        EventResult::Value(value) if !(MIN_TEMPERATURE_C..=MAX_TEMPERATURE_C).contains(&value) => (
            None,
            Quality::Invalid,
            Some(format!("原始温度{value}°C超出显示范围-40…150°C")),
        ),
        EventResult::Value(0.0) => (
            Some(0.0),
            Quality::Suspect,
            Some("原始0°C，疑似未启用或占位；不能确认实际温度".into()),
        ),
        EventResult::Value(value) => (Some(value), Quality::Valid, None),
    }
}

#[derive(Default)]
struct Collector {
    ids: HashSet<u64>,
    products: HashMap<String, usize>,
}
impl Collector {
    fn push(
        &mut self,
        batch: &mut Batch,
        diagnostics: &mut Diagnostics,
        id: Option<u64>,
        product: Option<String>,
        value: EventResult,
    ) {
        let Some(id) = id.filter(|id| *id != 0) else {
            batch.truncated = true;
            diagnostics.push("HID服务缺少可信registry ID，已省略；未生成不稳定序号身份");
            return;
        };
        if !self.ids.insert(id) {
            batch.truncated = true;
            diagnostics.push(format!(
                "HID registry ID {id:016x}重复出现，已省略重复服务引用"
            ));
            return;
        }
        if let Some(product) = &product {
            *self.products.entry(product.clone()).or_default() += 1;
        }
        let (temperature_c, quality, message) = classify(value);
        batch.readings.push(Reading {
            source_id: format!("hid:registry:{id:016x}"),
            registry_id: id,
            product,
            temperature_c,
            quality,
            message,
        });
    }
    fn finish(self, batch: &mut Batch, diagnostics: &mut Diagnostics) {
        let duplicates = self.products.values().filter(|count| **count > 1).count();
        if duplicates > 0 {
            diagnostics.push(format!("有{duplicates}个Product对应多个HID服务；已按各自registry ID全部保留，不能按Product覆盖或假定唯一物理源"));
        }
        batch.readings.sort_by_key(|reading| reading.registry_id);
    }
}

#[cfg(all(target_os = "macos", target_pointer_width = "64"))]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_void, CStr};
    type Cf = *const c_void;
    type TypeId = usize;
    #[repr(C)]
    struct CfRange {
        location: isize,
        length: isize,
    }

    // Public SDK CFBase/CFDictionary signatures: CFIndex is signed long;
    // CFTypeID unsigned long; Boolean is UInt8; callbacks are pointer-sized.
    #[repr(C)]
    struct KeyCallbacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        description: *const c_void,
        equal: *const c_void,
        hash: *const c_void,
    }
    #[repr(C)]
    struct ValueCallbacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        description: *const c_void,
        equal: *const c_void,
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Cf);
        fn CFGetTypeID(value: Cf) -> TypeId;
        fn CFStringGetTypeID() -> TypeId;
        fn CFNumberGetTypeID() -> TypeId;
        fn CFArrayGetTypeID() -> TypeId;
        fn CFDictionaryGetTypeID() -> TypeId;
        fn CFStringCreateWithCString(allocator: Cf, string: *const c_char, encoding: u32) -> Cf;
        fn CFStringGetLength(string: Cf) -> isize;
        fn CFStringGetBytes(
            string: Cf,
            range: CfRange,
            encoding: u32,
            loss_byte: u8,
            external: u8,
            buffer: *mut u8,
            max_length: isize,
            used_length: *mut isize,
        ) -> isize;
        fn CFNumberCreate(allocator: Cf, kind: isize, value: *const c_void) -> Cf;
        fn CFNumberGetValue(number: Cf, kind: isize, value: *mut c_void) -> u8;
        fn CFDictionaryCreate(
            allocator: Cf,
            keys: *const Cf,
            values: *const Cf,
            count: isize,
            key_callbacks: *const KeyCallbacks,
            value_callbacks: *const ValueCallbacks,
        ) -> Cf;
        fn CFArrayGetCount(array: Cf) -> isize;
        fn CFArrayGetValueAtIndex(array: Cf, index: isize) -> Cf;
        static kCFTypeDictionaryKeyCallBacks: KeyCallbacks;
        static kCFTypeDictionaryValueCallBacks: ValueCallbacks;
    }
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
    }
    struct Owned(Cf);
    impl Owned {
        fn checked(pointer: Cf, expected: TypeId, context: &str) -> Result<Self, String> {
            if pointer.is_null() {
                return Err(format!("{context}返回空对象"));
            }
            // Create/Copy transfers ownership even when an unexpected CF type is returned.
            let object = Self(pointer);
            if unsafe { CFGetTypeID(pointer) } != expected {
                return Err(format!("{context}返回非预期CF类型"));
            }
            Ok(object)
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                CFRelease(self.0);
            }
        }
    }
    struct Library(*mut c_void);
    impl Library {
        fn open() -> Result<Self, String> {
            // macOS dlfcn.h: RTLD_LAZY=1, RTLD_LOCAL=4. Fixed system path only.
            let handle = unsafe {
                dlopen(
                    c"/System/Library/Frameworks/IOKit.framework/IOKit".as_ptr(),
                    1 | 4,
                )
            };
            if handle.is_null() {
                return Err("无法运行时加载系统IOKit框架".into());
            }
            Ok(Self(handle))
        }
        fn symbol(&self, name: &CStr) -> Result<*mut c_void, String> {
            let address = unsafe { dlsym(self.0, name.as_ptr()) };
            if address.is_null() {
                return Err(format!(
                    "IOKit缺少运行时符号{}，HID provider不可用",
                    name.to_string_lossy()
                ));
            }
            Ok(address)
        }
    }
    impl Drop for Library {
        fn drop(&mut self) {
            unsafe {
                dlclose(self.0);
            }
        }
    }

    type Create = unsafe extern "C" fn(Cf) -> Cf;
    type SetMatching = unsafe extern "C" fn(Cf, Cf) -> i32;
    type CopyServices = unsafe extern "C" fn(Cf) -> Cf;
    type CopyProperty = unsafe extern "C" fn(Cf, Cf) -> Cf;
    // Private ABI declarations cross-checked with Chromium 123 m1_sensors_mac.mm.
    type CopyEvent = unsafe extern "C" fn(Cf, i64, i32, i64) -> Cf;
    type FloatValue = unsafe extern "C" fn(Cf, i32) -> f64;
    type GetTypeId = unsafe extern "C" fn() -> TypeId;
    // Public SDK declares GetRegistryID as borrowed CFTypeRef, not uint64_t.
    type RegistryId = unsafe extern "C" fn(Cf) -> Cf;
    struct Api {
        create: Create,
        set_matching: SetMatching,
        copy_services: CopyServices,
        copy_property: CopyProperty,
        copy_event: CopyEvent,
        float_value: FloatValue,
        client_type: GetTypeId,
        service_type: GetTypeId,
        event_type: GetTypeId,
        registry_id: RegistryId,
        _library: Library,
    }
    impl Api {
        fn load() -> Result<Self, String> {
            let library = Library::open()?;
            macro_rules! symbol {
                ($name:literal,$kind:ty) => {{
                    let pointer = library.symbol(
                        CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes())
                            .expect("literal symbol"),
                    )?;
                    // All function-pointer types have the pointer ABI on supported 64-bit Macs.
                    unsafe { std::mem::transmute::<*mut c_void, $kind>(pointer) }
                }};
            }
            Ok(Self {
                create: symbol!("IOHIDEventSystemClientCreate", Create),
                set_matching: symbol!("IOHIDEventSystemClientSetMatching", SetMatching),
                copy_services: symbol!("IOHIDEventSystemClientCopyServices", CopyServices),
                copy_property: symbol!("IOHIDServiceClientCopyProperty", CopyProperty),
                copy_event: symbol!("IOHIDServiceClientCopyEvent", CopyEvent),
                float_value: symbol!("IOHIDEventGetFloatValue", FloatValue),
                client_type: symbol!("IOHIDEventSystemClientGetTypeID", GetTypeId),
                service_type: symbol!("IOHIDServiceClientGetTypeID", GetTypeId),
                event_type: symbol!("IOHIDEventGetTypeID", GetTypeId),
                registry_id: symbol!("IOHIDServiceClientGetRegistryID", RegistryId),
                _library: library,
            })
        }
    }
    fn string(value: &CStr) -> Result<Owned, String> {
        Owned::checked(
            unsafe { CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), 0x08000100) },
            unsafe { CFStringGetTypeID() },
            "CFStringCreateWithCString",
        )
    }
    fn number(value: i32) -> Result<Owned, String> {
        Owned::checked(
            unsafe { CFNumberCreate(std::ptr::null(), 3, (&value as *const i32).cast()) },
            unsafe { CFNumberGetTypeID() },
            "CFNumberCreate",
        )
    }
    fn product(api: &Api, service: Cf, key: Cf) -> Result<Option<String>, String> {
        let raw = unsafe { (api.copy_property)(service, key) };
        if raw.is_null() {
            return Ok(None);
        }
        let product = Owned::checked(raw, unsafe { CFStringGetTypeID() }, "Product属性")?;
        bounded_string(&product).map(Some)
    }
    fn bounded_string(product: &Owned) -> Result<String, String> {
        let length = unsafe { CFStringGetLength(product.0) };
        if length < 0 || length as usize > MAX_PRODUCT_UTF16 {
            return Err("Product文本超出256个UTF-16单元上限".into());
        }
        let mut buffer = vec![0u8; MAX_PRODUCT_UTF16 * 4];
        let mut used = 0isize;
        let converted = unsafe {
            CFStringGetBytes(
                product.0,
                CfRange {
                    location: 0,
                    length,
                },
                0x08000100,
                0,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as isize,
                &mut used,
            )
        };
        if converted != length || used < 0 || used as usize > buffer.len() {
            return Err("Product无法转换为有界UTF-8文本".into());
        }
        buffer.truncate(used as usize);
        String::from_utf8(buffer).map_err(|_| "Product的UTF-8编码无效".into())
    }
    fn registry_id(api: &Api, service: Cf) -> Option<u64> {
        // Get returns a borrowed CFNumber: inspect it but do not CFRelease it.
        let number = unsafe { (api.registry_id)(service) };
        if number.is_null() || unsafe { CFGetTypeID(number) } != unsafe { CFNumberGetTypeID() } {
            return None;
        }
        let mut value = 0i64;
        if unsafe { CFNumberGetValue(number, 4, (&mut value as *mut i64).cast()) } == 0 {
            return None;
        }
        Some(value as u64).filter(|value| *value != 0)
    }
    fn temperature(api: &Api, service: Cf) -> EventResult {
        let raw = unsafe { (api.copy_event)(service, 15, 0, 0) };
        if raw.is_null() {
            return EventResult::Missing;
        }
        let Ok(event) = Owned::checked(raw, unsafe { (api.event_type)() }, "IOHID温度事件")
        else {
            return EventResult::Invalid("温度事件返回非预期CF类型");
        };
        EventResult::Value(unsafe { (api.float_value)(event.0, 0x000f0000) })
    }
    pub(super) fn collect(batch: &mut Batch, diagnostics: &mut Diagnostics) -> Result<(), String> {
        let api = Api::load()?;
        let usage_page_key = string(c"PrimaryUsagePage")?;
        let usage_key = string(c"PrimaryUsage")?;
        let product_key = string(c"Product")?;
        let page = number(0xff00)?;
        let usage = number(5)?;
        let keys = [usage_page_key.0, usage_key.0];
        let values = [page.0, usage.0];
        let dictionary = Owned::checked(
            unsafe {
                CFDictionaryCreate(
                    std::ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    2,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                )
            },
            unsafe { CFDictionaryGetTypeID() },
            "HID匹配字典",
        )?;
        let client = Owned::checked(
            unsafe { (api.create)(std::ptr::null()) },
            unsafe { (api.client_type)() },
            "IOHIDEventSystemClientCreate",
        )?;
        // Chromium's private declaration returns int but does not define an error
        // contract; don't invent success semantics from an undocumented value.
        unsafe {
            (api.set_matching)(client.0, dictionary.0);
        }
        let services = Owned::checked(
            unsafe { (api.copy_services)(client.0) },
            unsafe { CFArrayGetTypeID() },
            "IOHIDEventSystemClientCopyServices",
        )?;
        let count = unsafe { CFArrayGetCount(services.0) };
        if count < 0 {
            return Err("HID服务数组长度无效".into());
        }
        batch.available = true;
        batch.service_count = count as usize;
        if batch.service_count > MAX_SERVICES {
            batch.truncated = true;
            diagnostics.push(format!(
                "HID共{}个服务，仅读取前{MAX_SERVICES}个",
                batch.service_count
            ));
        }
        let mut collector = Collector::default();
        for index in 0..batch.service_count.min(MAX_SERVICES) {
            let service = unsafe { CFArrayGetValueAtIndex(services.0, index as isize) };
            // Borrowed service remains retained by the live services array.
            if service.is_null()
                || unsafe { CFGetTypeID(service) } != unsafe { (api.service_type)() }
            {
                batch.truncated = true;
                diagnostics.push(format!("HID服务数组第{index}项不是预期CF服务类型"));
                continue;
            }
            let id = registry_id(&api, service);
            let product = match product(&api, service, product_key.0) {
                Ok(Some(product)) => Some(product),
                Ok(None) => {
                    diagnostics.push(format!("HID服务{index}没有Product属性，保留无标签的原始源"));
                    None
                }
                Err(error) => {
                    batch.truncated = true;
                    diagnostics.push(format!("HID服务{index}：{error}，未猜测名称"));
                    None
                }
            };
            collector.push(batch, diagnostics, id, product, temperature(&api, service));
        }
        collector.finish(batch, diagnostics);
        if batch.readings.is_empty() {
            diagnostics
                .push("HID读取provider成功，但没有可保留的可信身份温度源；不代表设备无传感器");
        }
        let unusable = batch
            .readings
            .iter()
            .filter(|reading| matches!(reading.quality, Quality::Invalid | Quality::Unavailable))
            .count();
        if unusable > 0 {
            diagnostics.push(format!("本次有{unusable}个HID源无有效温度；失败未补为0"));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFStringCreateWithBytes(
                allocator: Cf,
                bytes: *const u8,
                length: isize,
                encoding: u32,
                external: u8,
            ) -> Cf;
        }
        fn cf_string(value: &str) -> Owned {
            Owned::checked(
                unsafe {
                    CFStringCreateWithBytes(
                        std::ptr::null(),
                        value.as_ptr(),
                        value.len() as isize,
                        0x08000100,
                        0,
                    )
                },
                unsafe { CFStringGetTypeID() },
                "CF-only test string",
            )
            .unwrap()
        }
        #[test]
        fn product_utf8_preserves_embedded_nul_unicode_and_size_boundaries() {
            // These tests only use CF values, never a HID service or device call.
            for value in [
                "".to_string(),
                "A\0B温度😀".into(),
                "x".repeat(MAX_PRODUCT_UTF16),
                "😀".repeat(MAX_PRODUCT_UTF16 / 2),
            ] {
                assert_eq!(bounded_string(&cf_string(&value)).unwrap(), value);
            }
            assert!(bounded_string(&cf_string(&"x".repeat(MAX_PRODUCT_UTF16 + 1))).is_err());
        }
        #[test]
        fn owned_cf_rejects_null_and_wrong_types_before_conversion() {
            assert!(Owned::checked(
                std::ptr::null(),
                unsafe { CFStringGetTypeID() },
                "null test"
            )
            .is_err());
            let value = 42i32;
            let raw = unsafe { CFNumberCreate(std::ptr::null(), 3, (&value as *const i32).cast()) };
            // Ownership transfers to checked() and is released on its error path.
            assert!(
                Owned::checked(raw, unsafe { CFStringGetTypeID() }, "wrong CF type test").is_err()
            );
            assert_eq!(std::mem::size_of::<CfRange>(), 16);
            assert_eq!(std::mem::size_of::<KeyCallbacks>(), 48);
            assert_eq!(std::mem::size_of::<ValueCallbacks>(), 40);
        }
    }
}

/// One fresh read attempt. No permission request, privileged helper or hardware
/// property write is performed. Identity/service failures never reuse old values.
pub fn sample() -> Batch {
    let mut batch = Batch::empty();
    let mut diagnostics = Diagnostics::default();
    #[cfg(all(target_os = "macos", target_pointer_width = "64"))]
    if let Err(error) = platform::collect(&mut batch, &mut diagnostics) {
        diagnostics.push(error);
    }
    #[cfg(not(all(target_os = "macos", target_pointer_width = "64")))]
    diagnostics.push("IOHID温度provider仅支持64位macOS");
    batch.captured_at_ms = now_ms();
    batch.diagnostics = diagnostics.finish();
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn null_event_is_missing_and_zero_is_a_retained_suspect() {
        assert_eq!(classify(EventResult::Missing).0, None);
        assert_eq!(classify(EventResult::Missing).1, Quality::Unavailable);
        for zero in [0.0, -0.0] {
            assert_eq!(classify(EventResult::Value(zero)).0, Some(0.0));
            assert_eq!(classify(EventResult::Value(zero)).1, Quality::Suspect);
        }
    }
    #[test]
    fn finite_temperature_boundaries_do_not_hide_invalid_values() {
        for value in [-40.0, -0.5, 42.5, 150.0] {
            assert_eq!(classify(EventResult::Value(value)).0, Some(value));
            assert_eq!(classify(EventResult::Value(value)).1, Quality::Valid);
        }
        for value in [-40.01, 150.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(classify(EventResult::Value(value)).0, None);
            assert_eq!(classify(EventResult::Value(value)).1, Quality::Invalid);
        }
        assert_eq!(
            classify(EventResult::Invalid("wrong CF type")).1,
            Quality::Invalid
        );
    }
    #[test]
    fn duplicate_products_retain_distinct_stable_source_ids() {
        let mut batch = Batch::empty();
        let mut diagnostics = Diagnostics::default();
        let mut collector = Collector::default();
        for id in [2, 1] {
            collector.push(
                &mut batch,
                &mut diagnostics,
                Some(id),
                Some("same Product".into()),
                EventResult::Value(40.0),
            );
        }
        collector.finish(&mut batch, &mut diagnostics);
        assert_eq!(batch.readings.len(), 2);
        assert_eq!(batch.readings[0].source_id, "hid:registry:0000000000000001");
        assert_ne!(batch.readings[0].source_id, batch.readings[1].source_id);
        assert!(!batch.truncated);
        assert!(diagnostics
            .finish()
            .iter()
            .any(|message| message.contains("多个HID服务")));
    }
    #[test]
    fn missing_or_repeated_registry_ids_never_masquerade_as_distinct_sources() {
        let mut batch = Batch::empty();
        let mut diagnostics = Diagnostics::default();
        let mut collector = Collector::default();
        for id in [None, Some(0), Some(7), Some(7)] {
            collector.push(&mut batch, &mut diagnostics, id, None, EventResult::Missing);
        }
        assert_eq!(batch.readings.len(), 1);
        assert!(batch.truncated);
        assert_eq!(batch.readings[0].product, None);
        assert_eq!(batch.readings[0].temperature_c, None);
        assert_eq!(batch.readings[0].quality.as_str(), "unavailable");
    }
    #[test]
    fn diagnostics_are_bounded_and_account_for_suppression() {
        let mut diagnostics = Diagnostics::default();
        for _ in 0..1000 {
            diagnostics.push("x".repeat(1000));
        }
        let entries = diagnostics.finish();
        assert_eq!(entries.len(), MAX_DIAGNOSTICS);
        assert!(entries.iter().all(|entry| entry.chars().count() <= 512));
        assert!(entries.last().unwrap().contains("937"));
        assert_eq!(Quality::Valid.as_str(), "valid");
        assert_eq!(Quality::Suspect.as_str(), "suspect");
        assert_eq!(Quality::Invalid.as_str(), "invalid");
    }
}
