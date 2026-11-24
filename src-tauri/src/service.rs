//! Fixed service operations. The WebView cannot supply identities, paths or plists.
#[cfg(any(test, not(target_os = "macos")))]
use serde_json::json;
use serde_json::Value;

#[derive(Clone, Copy)]
pub enum Operation {
    Status,
    Register,
    Unregister,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Register => "register",
            Self::Unregister => "unregister",
        }
    }
}

fn decode(bytes: &[u8], operation: Operation) -> Result<Value, String> {
    if bytes.len() > 65_536 {
        return Err("服务状态响应超出限制".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "服务状态格式无效")?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("operation").and_then(Value::as_str) != Some(operation.name())
        || value.get("hardware_write_enabled").and_then(Value::as_bool) != Some(false)
    {
        return Err("服务状态协议不匹配".into());
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
pub fn request(operation: Operation) -> Result<Value, String> {
    use std::ffi::{c_char, CStr};
    extern "C" {
        fn mf_service_status() -> *mut c_char;
        fn mf_service_register() -> *mut c_char;
        fn mf_service_unregister() -> *mut c_char;
        fn mf_service_free(value: *mut c_char);
    }
    struct Response(*mut c_char);
    impl Drop for Response {
        fn drop(&mut self) {
            // SAFETY: native returns an owned allocation; exactly this guard frees it.
            unsafe { mf_service_free(self.0) }
        }
    }
    // SAFETY: fixed local functions take no caller-supplied pointers or configuration.
    let pointer = unsafe {
        match operation {
            Operation::Status => mf_service_status(),
            Operation::Register => mf_service_register(),
            Operation::Unregister => mf_service_unregister(),
        }
    };
    if pointer.is_null() {
        return Err("服务管理器未返回状态".into());
    }
    let response = Response(pointer);
    // SAFETY: the local C ABI guarantees a live NUL-terminated UTF-8 JSON allocation.
    let bytes = unsafe { CStr::from_ptr(response.0) }.to_bytes();
    decode(bytes, operation)
}

#[cfg(not(target_os = "macos"))]
pub fn request(operation: Operation) -> Result<Value, String> {
    Ok(
        json!({"schema_version":1,"operation":operation.name(),"platform_supported":false,
        "service_state":"unsupported","hardware_write_enabled":false,"production_ready":false,
        "result":"blocked","reason":"unsupported_platform"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_contract_rejects_wrong_operation_version_or_write_claim() {
        let base = json!({"schema_version":1,"operation":"status","hardware_write_enabled":false});
        assert!(decode(base.to_string().as_bytes(), Operation::Status).is_ok());
        for (field, value) in [
            ("schema_version", json!(2)),
            ("operation", json!("register")),
            ("hardware_write_enabled", json!(true)),
        ] {
            let mut changed = base.clone();
            changed[field] = value;
            assert!(decode(changed.to_string().as_bytes(), Operation::Status).is_err());
        }
        assert!(decode(&vec![b' '; 65_537], Operation::Status).is_err());
        assert!(decode(b"null", Operation::Status).is_err());
    }
}
