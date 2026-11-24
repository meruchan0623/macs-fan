use crate::profile::Admission;
use crate::{AdapterError, Identity, Key, Metadata};
use std::time::{Duration, Instant};

pub(crate) trait Transport {
    fn identity(&mut self) -> Result<Identity, AdapterError>;
    fn info(&mut self, key: Key) -> Result<Metadata, AdapterError>;
    fn read(&mut self, key: Key, metadata: Metadata) -> Result<Vec<u8>, AdapterError>;
    fn write(
        &mut self,
        key: Key,
        metadata: Metadata,
        bytes: &[u8],
        admission: &Admission,
    ) -> Result<(), AdapterError>;
}

#[repr(C)]
#[derive(Default)]
struct Version {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}
#[repr(C)]
#[derive(Default)]
struct Limits {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    memory: u32,
}
#[repr(C)]
#[derive(Default)]
struct WireInfo {
    size: u32,
    kind: u32,
    attributes: u8,
    padding: [u8; 3],
}
#[repr(C)]
#[derive(Default)]
struct Packet {
    key: u32,
    version: Version,
    version_padding: [u8; 2],
    limits: Limits,
    info: WireInfo,
    result: u8,
    status: u8,
    command: u8,
    padding: u8,
    data: u32,
    bytes: [u8; 32],
}
const _: () = assert!(std::mem::size_of::<Packet>() == 80);

// Pure packet construction is tested without dispatching to any device.
fn write_packet(key: Key, metadata: Metadata, bytes: &[u8]) -> Result<Packet, AdapterError> {
    if bytes.len() != metadata.size as usize || bytes.is_empty() || bytes.len() > 32 {
        return Err(AdapterError::malformed(key, "invalid write length"));
    }
    let mut packet = Packet {
        key: key.code()?,
        command: 6,
        info: WireInfo {
            size: metadata.size,
            kind: u32::from_be_bytes(metadata.kind),
            attributes: metadata.attributes,
            padding: [0; 3],
        },
        ..Packet::default()
    };
    packet.bytes[..bytes.len()].copy_from_slice(bytes);
    Ok(packet)
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::{c_char, c_void};
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        pub fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
        pub fn IOServiceOpen(service: u32, task: u32, kind: u32, handle: *mut u32) -> i32;
        pub fn IOObjectRelease(object: u32) -> i32;
        pub fn IOServiceClose(handle: u32) -> i32;
        pub fn IOConnectCallStructMethod(
            handle: u32,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> i32;
    }
    unsafe extern "C" {
        pub static mach_task_self_: u32;
        pub fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            len: *mut usize,
            new: *mut c_void,
            new_len: usize,
        ) -> i32;
    }
}

pub(crate) struct RealTransport {
    handle: u32,
    write_attempts: u64,
}
impl RealTransport {
    #[cfg(target_os = "macos")]
    pub fn open() -> Result<Self, AdapterError> {
        let matching = unsafe { ffi::IOServiceMatching(c"AppleSMC".as_ptr()) };
        if matching.is_null() {
            return Err(AdapterError::Unavailable {
                reason: "IOServiceMatching returned null".into(),
            });
        }
        let service = unsafe { ffi::IOServiceGetMatchingService(0, matching) };
        if service == 0 {
            return Err(AdapterError::Unavailable {
                reason: "AppleSMC service not found".into(),
            });
        }
        let mut handle = 0;
        let result = unsafe { ffi::IOServiceOpen(service, ffi::mach_task_self_, 0, &mut handle) };
        unsafe {
            ffi::IOObjectRelease(service);
        }
        if result != 0 || handle == 0 {
            if handle != 0 {
                unsafe {
                    ffi::IOServiceClose(handle);
                }
            }
            return Err(AdapterError::IoKit {
                code: result as u32,
            });
        }
        Ok(Self {
            handle,
            write_attempts: 0,
        })
    }
    #[cfg(not(target_os = "macos"))]
    pub fn open() -> Result<Self, AdapterError> {
        Err(AdapterError::UnsupportedPlatform)
    }

    pub fn write_attempts(&self) -> u64 {
        self.write_attempts
    }

    fn call(&self, key: Key, input: Packet) -> Result<Packet, AdapterError> {
        self.call_raw(&key.name()?, input)
    }

    fn call_raw(&self, key: &str, input: Packet) -> Result<Packet, AdapterError> {
        let mut output = Packet::default();
        let mut output_size = std::mem::size_of::<Packet>();
        let started = Instant::now();
        #[cfg(target_os = "macos")]
        let result = unsafe {
            ffi::IOConnectCallStructMethod(
                self.handle,
                2,
                (&input as *const Packet).cast(),
                std::mem::size_of::<Packet>(),
                (&mut output as *mut Packet).cast(),
                &mut output_size,
            )
        };
        #[cfg(not(target_os = "macos"))]
        let result = {
            let _ = (&input, &mut output, &mut output_size);
            -1i32
        };
        // A synchronous kernel call cannot be cancelled here. Late completion is
        // never reported as success; helper isolation is required for a hard deadline.
        if started.elapsed() > Duration::from_millis(250) {
            return Err(AdapterError::Timeout);
        }
        if result != 0 {
            return Err(AdapterError::IoKit {
                code: result as u32,
            });
        }
        if output_size != 80 {
            return Err(AdapterError::Malformed {
                key: key.into(),
                reason: "SMC response is not 80 bytes".into(),
            });
        }
        if output.result == 0x84 {
            return Err(AdapterError::MissingKey { key: key.into() });
        }
        if output.result != 0 {
            return Err(AdapterError::Smc {
                key: key.into(),
                code: output.result,
            });
        }
        Ok(output)
    }
}

impl Transport for RealTransport {
    fn identity(&mut self) -> Result<Identity, AdapterError> {
        #[cfg(target_os = "macos")]
        {
            let mut len = 0usize;
            let result = unsafe {
                ffi::sysctlbyname(
                    c"hw.model".as_ptr(),
                    std::ptr::null_mut(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if result != 0 || len == 0 || len > 256 {
                return Err(AdapterError::Unavailable {
                    reason: "sysctl hw.model size failed".into(),
                });
            }
            let mut data = vec![0u8; len];
            let result = unsafe {
                ffi::sysctlbyname(
                    c"hw.model".as_ptr(),
                    data.as_mut_ptr().cast(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if result != 0 || len == 0 || len > data.len() {
                return Err(AdapterError::Unavailable {
                    reason: "sysctl hw.model read failed".into(),
                });
            }
            data.truncate(len);
            if data.last() == Some(&0) {
                data.pop();
            }
            let model = String::from_utf8(data).map_err(|_| AdapterError::Unavailable {
                reason: "hw.model is not UTF-8".into(),
            })?;
            if model.is_empty() {
                return Err(AdapterError::Unavailable {
                    reason: "hw.model is empty".into(),
                });
            }
            Ok(Identity {
                model,
                architecture: std::env::consts::ARCH.into(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(AdapterError::UnsupportedPlatform)
        }
    }
    fn info(&mut self, key: Key) -> Result<Metadata, AdapterError> {
        let packet = Packet {
            key: key.code()?,
            command: 9,
            ..Packet::default()
        };
        let info = self.call(key, packet)?.info;
        if info.size == 0 || info.size > 32 {
            return Err(AdapterError::malformed(key, "SMC key size outside 1..32"));
        }
        Ok(Metadata {
            size: info.size,
            kind: info.kind.to_be_bytes(),
            attributes: info.attributes,
        })
    }
    fn read(&mut self, key: Key, metadata: Metadata) -> Result<Vec<u8>, AdapterError> {
        if metadata.size == 0 || metadata.size > 32 {
            return Err(AdapterError::malformed(key, "invalid read length"));
        }
        let packet = Packet {
            key: key.code()?,
            command: 5,
            info: WireInfo {
                size: metadata.size,
                ..WireInfo::default()
            },
            ..Packet::default()
        };
        Ok(self.call(key, packet)?.bytes[..metadata.size as usize].to_vec())
    }
    fn write(
        &mut self,
        key: Key,
        metadata: Metadata,
        bytes: &[u8],
        admission: &Admission,
    ) -> Result<(), AdapterError> {
        admission.verify_real_registry()?;
        admission.verify(&self.identity()?, key, metadata)?;
        if self.info(key)? != metadata {
            return Err(AdapterError::MetadataChanged { key: key.name()? });
        }
        let packet = write_packet(key, metadata, bytes)?;
        self.write_attempts = self.write_attempts.saturating_add(1);
        self.call(key, packet)?;
        Ok(())
    }
}

impl Drop for RealTransport {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            ffi::IOServiceClose(self.handle);
        }
    }
}

// Separate read-only surface: no admission, actuator key, or write opcode is exposed.
pub(crate) struct TemperatureTransport(RealTransport);
impl TemperatureTransport {
    pub(crate) fn open() -> Result<Self, AdapterError> {
        RealTransport::open().map(Self)
    }
}
impl crate::temperature::ReadIo for TemperatureTransport {
    fn identity(&mut self) -> Result<Identity, AdapterError> {
        self.0.identity()
    }
    fn info(&mut self, key: &str) -> Result<Metadata, AdapterError> {
        let code = crate::temperature::key_code(key)?;
        let info = self
            .0
            .call_raw(
                key,
                Packet {
                    key: code,
                    command: 9,
                    ..Packet::default()
                },
            )?
            .info;
        Ok(Metadata {
            size: info.size,
            kind: info.kind.to_be_bytes(),
            attributes: info.attributes,
        })
    }
    fn read(&mut self, key: &str, metadata: Metadata) -> Result<Vec<u8>, AdapterError> {
        let code = crate::temperature::key_code(key)?;
        if metadata.size == 0 || metadata.size > 32 {
            return Err(AdapterError::Malformed {
                key: key.into(),
                reason: "invalid temperature read length".into(),
            });
        }
        let output = self.0.call_raw(
            key,
            Packet {
                key: code,
                command: 5,
                info: WireInfo {
                    size: metadata.size,
                    ..WireInfo::default()
                },
                ..Packet::default()
            },
        )?;
        Ok(output.bytes[..metadata.size as usize].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_layout_and_explicit_padding() {
        assert_eq!(std::mem::size_of::<Packet>(), 80);
        assert_eq!(std::mem::offset_of!(Packet, info), 28);
        assert_eq!(std::mem::offset_of!(Packet, command), 42);
        assert_eq!(std::mem::offset_of!(Packet, data), 44);
        assert_eq!(std::mem::offset_of!(Packet, bytes), 48);
    }

    #[test]
    fn command_six_has_exact_key_length_type_bytes_and_zero_padding() {
        use macs_fan_control_core::FanId;
        let metadata = Metadata {
            size: 2,
            kind: *b"fpe2",
            attributes: 0xd4,
        };
        let packet = write_packet(Key::Target(FanId(1)), metadata, &[0x1f, 0x41]).unwrap();
        assert_eq!(packet.command, 6);
        assert_eq!(packet.key, u32::from_be_bytes(*b"F1Tg"));
        assert_eq!(packet.info.size, 2);
        assert_eq!(packet.info.kind, u32::from_be_bytes(*b"fpe2"));
        assert_eq!(packet.info.attributes, 0xd4);
        assert_eq!(&packet.bytes[..2], &[0x1f, 0x41]);
        assert!(packet.bytes[2..].iter().all(|byte| *byte == 0));
        assert_eq!(packet.info.padding, [0; 3]);
        assert_eq!(packet.version_padding, [0; 2]);
        assert_eq!(packet.padding, 0);
        assert!(write_packet(Key::Target(FanId(1)), metadata, &[0]).is_err());
        assert!(write_packet(Key::Target(FanId(16)), metadata, &[0, 0]).is_err());
    }
}
