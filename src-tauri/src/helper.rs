//! Fixed, authenticated helper inspection. This module exposes no write command.
#[cfg(any(test, not(target_os = "macos")))]
use serde_json::json;
use serde_json::Value;

fn decode(bytes: &[u8]) -> Result<Value, String> {
    // Native caps received NSData at 1 MiB; allow room for its result envelope.
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("辅助服务响应超出限制".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "辅助服务响应格式无效")?;
    let fail = || "辅助服务响应协议不匹配".to_string();
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("operation").and_then(Value::as_str) != Some("inspect_hardware")
        || value.get("hardware_write_enabled").and_then(Value::as_bool) != Some(false)
        || value
            .get("production_control_ready")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(fail());
    }
    let authenticated = value
        .get("authenticated_response")
        .and_then(Value::as_bool)
        .ok_or_else(fail)?;
    let result = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(fail)?;
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(fail)?;
    if reason.is_empty() || reason.len() > 256 {
        return Err(fail());
    }
    if !authenticated {
        if !matches!(result, "blocked" | "unavailable" | "busy")
            || value.get("response").is_some_and(|v| !v.is_null())
        {
            return Err(fail());
        }
        return Ok(value);
    }
    let response = value.get("response").ok_or_else(fail)?;
    // Inspection never grants a plan authorization. The nested response still
    // reports the actual host execution kind, including an admitted write path.
    let backend = response.get("backend").and_then(Value::as_str);
    let write_path = match backend {
        Some("smc_control") => true,
        Some("smc_readonly" | "unavailable") => false,
        _ => return Err(fail()),
    };
    if response.get("version").and_then(Value::as_u64) != Some(1)
        || response.get("dry_run").and_then(Value::as_bool) != Some(!write_path)
        || response
            .get("hardware_write_enabled")
            .and_then(Value::as_bool)
            != Some(write_path)
    {
        return Err(fail());
    }
    if let Some(writes) = response
        .get("hardware_write_attempts")
        .filter(|v| !v.is_null())
    {
        let count = writes.as_u64().ok_or_else(fail)?;
        if !write_path && count != 0 {
            return Err(fail());
        }
    }
    match response.get("ok").and_then(Value::as_bool) {
        Some(true) => {
            let inspection = &response["result"]["inspection"];
            if result != "observed"
                || backend == Some("unavailable")
                || response["result"]["kind"].as_str() != Some("hardware_inspection")
                || response["hardware_reads"].as_bool() != Some(true)
                || !inspection.is_object()
                || inspection["hardwareProfileVerified"].as_bool().is_none()
                || (!write_path && inspection["hardwareProfileVerified"].as_bool() != Some(false))
                || inspection["hardwareWriteAttempts"].as_u64().is_none()
                || response["hardware_write_attempts"].as_u64()
                    != inspection["hardwareWriteAttempts"].as_u64()
                || response.get("error").is_some()
            {
                return Err(fail());
            }
        }
        Some(false) => {
            let code = response["error"]["code"].as_str().ok_or_else(fail)?;
            if result != "helper_error"
                || code.is_empty()
                || code.len() > 128
                || response.get("result").is_some()
            {
                return Err(fail());
            }
        }
        None => return Err(fail()),
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
pub fn inspect() -> Result<Value, String> {
    use std::ffi::{c_char, CStr};
    extern "C" {
        fn mf_helper_inspect() -> *mut c_char;
        fn mf_helper_client_free(value: *mut c_char);
    }
    struct Response(*mut c_char);
    impl Drop for Response {
        fn drop(&mut self) {
            // SAFETY: one native allocation, owned by this guard until decoding ends.
            unsafe { mf_helper_client_free(self.0) }
        }
    }
    // SAFETY: fixed no-argument ABI; paths, commands and requirements cannot be supplied.
    let pointer = unsafe { mf_helper_inspect() };
    if pointer.is_null() {
        return Err("辅助服务未返回状态".into());
    }
    let response = Response(pointer);
    // SAFETY: our native ABI returns a bounded, live NUL-terminated allocation.
    let bytes = unsafe { CStr::from_ptr(response.0) }.to_bytes();
    decode(bytes)
}

#[cfg(not(target_os = "macos"))]
pub fn inspect() -> Result<Value, String> {
    Ok(json!({"schema_version":1,"operation":"inspect_hardware",
        "hardware_write_enabled":false,"production_control_ready":false,
        "authenticated_response":false,"result":"blocked","reason":"unsupported_platform"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blocked() -> Value {
        json!({"schema_version":1,"operation":"inspect_hardware",
            "hardware_write_enabled":false,"production_control_ready":false,
            "authenticated_response":false,"result":"blocked","reason":"production_team_unconfigured"})
    }
    fn observed() -> Value {
        let mut value = blocked();
        value["authenticated_response"] = json!(true);
        value["result"] = json!("observed");
        value["reason"] = json!("authenticated_inspection");
        value["response"] = json!({"version":1,"dry_run":true,"hardware_write_enabled":false,
            "ok":true,"backend":"smc_readonly","hardware_reads":true,"hardware_write_attempts":0,"result":{"kind":"hardware_inspection",
            "inspection":{"hardwareProfileVerified":false,"hardwareWriteAttempts":0}}});
        value
    }
    fn valid(value: &Value) -> bool {
        decode(value.to_string().as_bytes()).is_ok()
    }
    #[test]
    fn absence_of_credentials_is_a_blocked_result_not_authentication() {
        assert!(valid(&blocked()));
        for result in ["observed", "helper_error", "ready", ""] {
            let mut value = blocked();
            value["result"] = json!(result);
            assert!(!valid(&value));
        }
        let mut value = blocked();
        value["response"] = observed()["response"].clone();
        assert!(!valid(&value));
    }
    #[test]
    fn inspection_never_promotes_control_or_accepts_another_operation() {
        assert!(valid(&observed()));
        for (key, replacement) in [
            ("schema_version", json!(2)),
            ("operation", json!("acquire")),
            ("hardware_write_enabled", json!(true)),
            ("production_control_ready", json!(true)),
            ("authenticated_response", json!(1)),
        ] {
            let mut value = observed();
            value[key] = replacement;
            assert!(!valid(&value), "{key}");
        }
        let mut value = observed();
        value["response"]["result"]["kind"] = json!("lease");
        assert!(!valid(&value));
    }
    #[test]
    fn readonly_backend_rejects_write_or_verified_profile_claims() {
        for (path, replacement) in [
            ("/response/hardware_write_enabled", json!(true)),
            ("/response/dry_run", json!(false)),
            ("/response/backend", json!("fake")),
            (
                "/response/result/inspection/hardwareProfileVerified",
                json!(true),
            ),
            (
                "/response/result/inspection/hardwareWriteAttempts",
                json!(1),
            ),
        ] {
            let mut value = observed();
            *value.pointer_mut(path).unwrap() = replacement;
            assert!(!valid(&value), "{path}");
        }
    }
    #[test]
    fn authenticated_helper_failure_is_not_an_observation() {
        let mut value = observed();
        value["result"] = json!("helper_error");
        value["response"] = json!({"version":1,"dry_run":true,"hardware_write_enabled":false,
            "ok":false,"backend":"unavailable","error":{"code":"actuator_unavailable"}});
        assert!(valid(&value));
        value["result"] = json!("observed");
        assert!(!valid(&value));
        value["result"] = json!("helper_error");
        value["response"]["result"] = json!({"kind":"lease"});
        assert!(!valid(&value));
    }
    #[test]
    fn control_host_observation_preserves_rpc_readonly_boundary() {
        let mut value = observed();
        value["response"]["backend"] = json!("smc_control");
        value["response"]["dry_run"] = json!(false);
        value["response"]["hardware_write_enabled"] = json!(true);
        for verified in [false, true] {
            value["response"]["result"]["inspection"]["hardwareProfileVerified"] = json!(verified);
            for writes in [0, 1, u64::MAX] {
                value["response"]["hardware_write_attempts"] = json!(writes);
                value["response"]["result"]["inspection"]["hardwareWriteAttempts"] = json!(writes);
                assert!(valid(&value));
                assert_eq!(value["hardware_write_enabled"], false);
                assert_eq!(value["production_control_ready"], false);
            }
        }
        for (path, replacement) in [
            ("/response/backend", json!("fake")),
            ("/response/backend", json!("unavailable")),
            ("/response/dry_run", json!(true)),
            ("/response/hardware_write_enabled", json!(false)),
            ("/response/hardware_write_attempts", json!(-1)),
            ("/response/hardware_write_attempts", json!(1.5)),
            ("/response/hardware_write_attempts", json!(true)),
            ("/response/hardware_write_attempts", json!(0)),
            (
                "/response/result/inspection/hardwareProfileVerified",
                json!(1),
            ),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(path).unwrap() = replacement;
            assert!(!valid(&invalid), "{path}");
        }
    }
    #[test]
    fn inspection_failure_requires_consistent_execution_kind() {
        for (backend, writable) in [
            ("smc_readonly", false),
            ("unavailable", false),
            ("smc_control", true),
        ] {
            let mut value = observed();
            value["result"] = json!("helper_error");
            value["response"] = json!({"version":1,"backend":backend,"dry_run":!writable,
                "hardware_write_enabled":writable,"ok":false,"error":{"code":"actuator_unavailable"}});
            assert!(valid(&value));
            value["response"]["hardware_write_enabled"] = json!(!writable);
            assert!(!valid(&value));
        }
        let mut value = observed();
        value["response"].as_object_mut().unwrap().remove("backend");
        assert!(!valid(&value));
    }
    #[test]
    fn malformed_oversize_or_unbounded_text_is_rejected() {
        for bytes in [
            b"null".as_slice(),
            b"[]",
            b"{",
            b"{\"schema_version\":true}",
        ] {
            assert!(decode(bytes).is_err());
        }
        assert!(decode(&vec![b' '; 2 * 1024 * 1024 + 1]).is_err());
        for reason in [String::new(), "x".repeat(257)] {
            let mut value = blocked();
            value["reason"] = json!(reason);
            assert!(!valid(&value));
        }
    }
}
