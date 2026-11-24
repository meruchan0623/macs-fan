//! Independent read-only implementation of the community-researched AppleSMC ABI.
//! Protocol cross-check: https://github.com/exelban/stats/blob/master/SMC/smc.swift
//! (MIT, Serhiy Mytrovtsiy). No implementation or vendor helper is incorporated.
use std::collections::HashMap;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Version {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PowerLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct KeyInfo {
    pub size: u32,
    pub kind: u32,
    attributes: u8,
    padding: [u8; 3],
}

#[repr(C)]
#[derive(Default)]
struct Packet {
    key: u32,
    version: Version,
    version_padding: [u8; 2],
    power_limit: PowerLimit,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    command: u8,
    command_padding: u8,
    data: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<Packet>() == 80);

pub fn fourcc(value: &str) -> Result<u32, String> {
    let bytes: [u8; 4] = value
        .as_bytes()
        .try_into()
        .map_err(|_| "SMC 键必须恰为4字节".to_string())?;
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        return Err("SMC 键含非打印字符".into());
    }
    Ok(u32::from_be_bytes(bytes))
}

pub fn type_name(kind: u32) -> String {
    String::from_utf8_lossy(&kind.to_be_bytes()).into_owned()
}

pub fn decode(info: KeyInfo, bytes: &[u8]) -> Result<f64, String> {
    if info.size as usize != bytes.len() || bytes.is_empty() || bytes.len() > 32 {
        return Err(format!(
            "数据长度不符：声明 {}，实际 {}",
            info.size,
            bytes.len()
        ));
    }
    let kind = type_name(info.kind);
    let expected = match kind.as_str() {
        "flt " | "ui32" => 4,
        "sp78" | "fpe2" | "ui16" => 2,
        "ui8 " => 1,
        _ => return Err(format!("不支持编码 {kind:?}")),
    };
    if bytes.len() != expected {
        return Err(format!(
            "编码 {kind:?} 需要 {expected} 字节，收到 {}",
            bytes.len()
        ));
    }
    let value = match kind.as_str() {
        // AppleSMC flt is a native IEEE-754 float on supported little-endian Macs.
        "flt " => f32::from_le_bytes(bytes.try_into().unwrap()) as f64,
        "sp78" => i16::from_be_bytes(bytes.try_into().unwrap()) as f64 / 256.0,
        "fpe2" => u16::from_be_bytes(bytes.try_into().unwrap()) as f64 / 4.0,
        "ui8 " => bytes[0] as f64,
        "ui16" => u16::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "ui32" => u32::from_be_bytes(bytes.try_into().unwrap()) as f64,
        _ => unreachable!(),
    };
    if !value.is_finite() {
        return Err("读数为 NaN 或无穷值".into());
    }
    Ok(value)
}

/// Only read operations can be represented. There is deliberately no write opcode.
enum ReadCommand {
    Bytes = 5,
    Index = 8,
    Info = 9,
}

pub struct Connection {
    handle: u32,
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::{c_char, c_void};
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        pub fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
        pub fn IOServiceOpen(service: u32, task: u32, kind: u32, connection: *mut u32) -> i32;
        pub fn IOObjectRelease(object: u32) -> i32;
        pub fn IOServiceClose(connection: u32) -> i32;
        pub fn IOConnectCallStructMethod(
            connection: u32,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> i32;
    }
    unsafe extern "C" {
        pub static mach_task_self_: u32;
    }
}

impl Connection {
    #[cfg(target_os = "macos")]
    pub fn open() -> Result<Self, String> {
        // Matching dictionary ownership is consumed by GetMatchingService.
        let matching = unsafe { ffi::IOServiceMatching(c"AppleSMC".as_ptr()) };
        if matching.is_null() {
            return Err("无法创建 AppleSMC 服务匹配".into());
        }
        let service = unsafe { ffi::IOServiceGetMatchingService(0, matching) };
        if service == 0 {
            return Err("未找到 AppleSMC 服务".into());
        }
        let mut handle = 0;
        let result = unsafe { ffi::IOServiceOpen(service, ffi::mach_task_self_, 0, &mut handle) };
        unsafe {
            ffi::IOObjectRelease(service);
        }
        if result != 0 {
            if handle != 0 {
                unsafe {
                    ffi::IOServiceClose(handle);
                }
            }
            return Err(format!("AppleSMC 打开失败：IOKit 0x{:08x}", result as u32));
        }
        if handle == 0 {
            return Err("AppleSMC 返回空连接".into());
        }
        Ok(Self { handle })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn open() -> Result<Self, String> {
        Err("仅支持 macOS AppleSMC".into())
    }

    fn call(
        &self,
        command: ReadCommand,
        key: u32,
        data: u32,
        info: KeyInfo,
    ) -> Result<Packet, String> {
        let input = Packet {
            key,
            command: command as u8,
            data,
            key_info: info,
            ..Packet::default()
        };
        let mut output = Packet::default();
        let mut size = std::mem::size_of::<Packet>();
        #[cfg(target_os = "macos")]
        let result = unsafe {
            ffi::IOConnectCallStructMethod(
                self.handle,
                2,
                (&input as *const Packet).cast(),
                std::mem::size_of::<Packet>(),
                (&mut output as *mut Packet).cast(),
                &mut size,
            )
        };
        #[cfg(not(target_os = "macos"))]
        let result = {
            let _ = (&input, &mut output, &mut size);
            -1i32
        };
        if result != 0 {
            return Err(format!("IOKit 0x{:08x}", result as u32));
        }
        if size != std::mem::size_of::<Packet>() {
            return Err(format!("SMC 响应长度 {size}，预期80"));
        }
        if output.result != 0 {
            return Err(format!("SMC 结果码 0x{:02x}", output.result));
        }
        Ok(output)
    }

    pub fn info(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<KeyInfo, String> {
        if let Some(info) = cache.get(key) {
            return Ok(*info);
        }
        let info = self
            .call(ReadCommand::Info, fourcc(key)?, 0, KeyInfo::default())?
            .key_info;
        if info.size == 0 || info.size > 32 {
            return Err(format!("无效数据长度 {}", info.size));
        }
        cache.insert(key.to_string(), info);
        Ok(info)
    }

    pub fn number(&self, key: &str, cache: &mut HashMap<String, KeyInfo>) -> Result<f64, String> {
        let info = self.info(key, cache)?;
        let result = self.call(ReadCommand::Bytes, fourcc(key)?, 0, info)?;
        decode(info, &result.bytes[..info.size as usize])
    }

    pub fn key_at(&self, index: u32) -> Result<String, String> {
        let key = type_name(
            self.call(ReadCommand::Index, 0, index, KeyInfo::default())?
                .key,
        );
        fourcc(&key)?;
        Ok(key)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            ffi::IOServiceClose(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(kind: &str, bytes: &[u8]) -> Result<f64, String> {
        decode(
            KeyInfo {
                size: bytes.len() as u32,
                kind: fourcc(kind).unwrap(),
                attributes: 0,
                padding: [0; 3],
            },
            bytes,
        )
    }
    #[test]
    fn abi_layout_matches_driver() {
        assert_eq!(std::mem::size_of::<Packet>(), 80);
        assert_eq!(std::mem::offset_of!(Packet, key_info), 28);
        assert_eq!(std::mem::offset_of!(Packet, result), 40);
        assert_eq!(std::mem::offset_of!(Packet, command), 42);
        assert_eq!(std::mem::offset_of!(Packet, data), 44);
        assert_eq!(std::mem::offset_of!(Packet, bytes), 48);
    }
    #[test]
    fn typed_values_preserve_signed_and_fractional_parts() {
        assert_eq!(value("sp78", &[0xff, 0x80]).unwrap(), -0.5);
        assert_eq!(value("sp78", &[0x2a, 0x80]).unwrap(), 42.5);
        assert_eq!(value("fpe2", &[0x1f, 0x41]).unwrap(), 2000.25);
        assert_eq!(value("flt ", &42.5f32.to_le_bytes()).unwrap(), 42.5);
        assert_eq!(value("ui8 ", &[0]).unwrap(), 0.0);
        assert_eq!(value("ui16", &[1, 2]).unwrap(), 258.0);
        assert_eq!(value("ui32", &[0, 1, 0, 0]).unwrap(), 65536.0);
    }
    #[test]
    fn rejects_malformed_unknown_and_nonfinite_values() {
        assert!(value("flt ", &[0, 0]).is_err());
        assert!(value("flt ", &f32::NAN.to_le_bytes()).is_err());
        assert!(value("flt ", &f32::INFINITY.to_le_bytes()).is_err());
        assert!(value("xxxx", &[0, 0]).is_err());
        assert!(decode(
            KeyInfo {
                size: 4,
                kind: fourcc("ui32").unwrap(),
                attributes: 0,
                padding: [0; 3]
            },
            &[0, 0]
        )
        .is_err());
        assert!(fourcc("F10Ac").is_err());
        assert!(fourcc("温度").is_err());
    }
}
