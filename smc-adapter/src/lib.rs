//! Typed AppleSMC actuator. Public entry points cannot supply keys, profiles,
//! admission tokens, or validated capabilities. Real writes require an internal
//! reviewed hardware profile; none has yet passed restoration validation.
mod profile;
pub mod temperature;
mod transport;

use macs_fan_control_core::{
    Actuator, ActuatorError, Capabilities, FanId, HardwareState, Mode, ModeControl, ModelSupport,
    TargetEncoding, WriteMode,
};
use profile::{Admission, Profile, Route};
use serde::Serialize;
use transport::{RealTransport, Transport};

/// Read-only construction-time assessment. This is not a write admission token;
/// every Actuator write still revalidates the private compiled profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAdmission {
    NoReviewedProfile,
    ProfileValidationFailed,
    Admitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub model: String,
    pub architecture: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterError {
    UnsupportedPlatform,
    Unavailable { reason: String },
    IoKit { code: u32 },
    Smc { key: String, code: u8 },
    MissingKey { key: String },
    Malformed { key: String, reason: String },
    MetadataChanged { key: String },
    IdentityChanged,
    UnknownFan { fan: u16 },
    Denied { reason: String },
    ReadbackMismatch { key: String },
    Timeout,
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AdapterError {}
impl AdapterError {
    fn malformed(key: Key, reason: &str) -> Self {
        Self::Malformed {
            key: key.name().unwrap_or_else(|_| "invalid fan".into()),
            reason: reason.into(),
        }
    }
    fn core(&self) -> ActuatorError {
        match self {
            Self::Timeout => ActuatorError::Timeout,
            Self::UnsupportedPlatform | Self::Denied { .. } | Self::UnknownFan { .. } => {
                ActuatorError::Unsupported
            }
            Self::Unavailable { .. } => ActuatorError::Unavailable,
            _ => ActuatorError::Io,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Metadata {
    pub size: u32,
    pub kind: [u8; 4],
    pub attributes: u8,
}
impl Metadata {
    const UI8: Self = Self {
        size: 1,
        kind: *b"ui8 ",
        attributes: 0,
    };
    const UI16: Self = Self {
        size: 2,
        kind: *b"ui16",
        attributes: 0,
    };
    fn same_encoding(self, other: Self) -> bool {
        self.kind == other.kind && self.size == other.size
    }
    fn target_encoding(self) -> TargetEncoding {
        match (self.kind, self.size) {
            (kind, 2) if kind == *b"fpe2" => TargetEncoding::Fpe2,
            (kind, 4) if kind == *b"flt " => TargetEncoding::Float32,
            _ => TargetEncoding::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Count,
    Mask,
    Gate,
    Current(FanId),
    Minimum(FanId),
    Maximum(FanId),
    Target(FanId),
    ModeUpper(FanId),
    ModeLower(FanId),
}
impl Key {
    fn name(self) -> Result<String, AdapterError> {
        let (fan, suffix) = match self {
            Self::Count => return Ok("FNum".into()),
            Self::Mask => return Ok("FS! ".into()),
            Self::Gate => return Ok("Ftst".into()),
            Self::Current(fan) => (fan, "Ac"),
            Self::Minimum(fan) => (fan, "Mn"),
            Self::Maximum(fan) => (fan, "Mx"),
            Self::Target(fan) => (fan, "Tg"),
            Self::ModeUpper(fan) => (fan, "Md"),
            Self::ModeLower(fan) => (fan, "md"),
        };
        if fan.0 >= 16 {
            return Err(AdapterError::UnknownFan { fan: fan.0 });
        }
        Ok(format!("F{:X}{suffix}", fan.0))
    }
    fn code(self) -> Result<u32, AdapterError> {
        let name = self.name()?;
        Ok(u32::from_be_bytes(
            name.as_bytes().try_into().expect("fixed key length"),
        ))
    }
}

fn decode(key: Key, metadata: Metadata, bytes: &[u8]) -> Result<f64, AdapterError> {
    if bytes.len() != metadata.size as usize {
        return Err(AdapterError::malformed(key, "size mismatch"));
    }
    let value = match (metadata.kind, bytes.len()) {
        (kind, 1) if kind == *b"ui8 " => bytes[0] as f64,
        (kind, 2) if kind == *b"ui16" => u16::from_be_bytes(bytes.try_into().unwrap()) as f64,
        (kind, 4) if kind == *b"ui32" => u32::from_be_bytes(bytes.try_into().unwrap()) as f64,
        (kind, 2) if kind == *b"fpe2" => u16::from_be_bytes(bytes.try_into().unwrap()) as f64 / 4.0,
        (kind, 4) if kind == *b"flt " => f32::from_le_bytes(bytes.try_into().unwrap()) as f64,
        _ => return Err(AdapterError::malformed(key, "unsupported type or size")),
    };
    if !value.is_finite() {
        return Err(AdapterError::malformed(key, "nonfinite reading"));
    }
    Ok(value)
}

fn encode_target(metadata: Metadata, rpm: f64) -> Result<Vec<u8>, AdapterError> {
    if !rpm.is_finite() || rpm < 0.0 {
        return Err(AdapterError::malformed(
            Key::Target(FanId(0)),
            "invalid RPM",
        ));
    }
    match metadata.target_encoding() {
        TargetEncoding::Fpe2 if rpm <= 16383.75 && (rpm * 4.0).fract() == 0.0 => {
            Ok(((rpm * 4.0) as u16).to_be_bytes().to_vec())
        }
        TargetEncoding::Float32 if (rpm as f32).is_finite() && (rpm as f32) as f64 == rpm => {
            Ok((rpm as f32).to_le_bytes().to_vec())
        }
        _ => Err(AdapterError::malformed(
            Key::Target(FanId(0)),
            "RPM cannot be represented exactly by target metadata",
        )),
    }
}

fn direct_key(route: Route, fan: FanId) -> Key {
    match route {
        Route::PerFan {
            lowercase: true, ..
        } => Key::ModeLower(fan),
        _ => Key::ModeUpper(fan),
    }
}

struct Inner<T> {
    transport: T,
    identity: Identity,
    profile: Option<&'static Profile>,
}
impl<T: Transport> Inner<T> {
    fn new(mut transport: T) -> Result<Self, AdapterError> {
        let identity = transport.identity()?;
        let profile = profile::select(&identity);
        Ok(Self {
            transport,
            identity,
            profile,
        })
    }
    fn into_readonly(mut self) -> Self {
        self.profile = None;
        self
    }
    fn check_identity(&mut self) -> Result<(), AdapterError> {
        if self.transport.identity()? != self.identity {
            return Err(AdapterError::IdentityChanged);
        }
        Ok(())
    }
    fn number(&mut self, key: Key) -> Result<(Metadata, f64), AdapterError> {
        let metadata = self.transport.info(key)?;
        let bytes = self.transport.read(key, metadata)?;
        Ok((metadata, decode(key, metadata, &bytes)?))
    }
    fn expected(&mut self, key: Key, expected: Metadata) -> Result<f64, AdapterError> {
        let metadata = self.transport.info(key)?;
        if metadata != expected {
            return Err(AdapterError::MetadataChanged { key: key.name()? });
        }
        let bytes = self.transport.read(key, metadata)?;
        decode(key, metadata, &bytes)
    }
    fn inventory(&mut self) -> Result<Vec<FanId>, AdapterError> {
        self.check_identity()?;
        let (metadata, count) = self.number(Key::Count)?;
        if metadata.kind != *b"ui8 "
            || metadata.size != 1
            || count.fract() != 0.0
            || !(0.0..=16.0).contains(&count)
        {
            return Err(AdapterError::malformed(
                Key::Count,
                "invalid fan count encoding/value",
            ));
        }
        Ok((0..count as u16).map(FanId).collect())
    }
    fn require_fan(&mut self, fan: FanId) -> Result<(), AdapterError> {
        if !self.inventory()?.contains(&fan) {
            return Err(AdapterError::UnknownFan { fan: fan.0 });
        }
        Ok(())
    }
    fn route(&mut self, fan: FanId) -> Result<Route, AdapterError> {
        if let Some(profile) = self.profile {
            return Ok(profile.route);
        }
        match self.transport.info(Key::Mask) {
            Ok(metadata) => {
                if metadata.kind != *b"ui16" || metadata.size != 2 {
                    return Err(AdapterError::malformed(Key::Mask, "mask is not ui16/2"));
                }
                return Ok(Route::SharedMask);
            }
            Err(AdapterError::MissingKey { .. }) => {}
            Err(error) => return Err(error),
        }
        match self.transport.info(Key::ModeUpper(fan)) {
            Ok(_) => Ok(Route::PerFan {
                lowercase: false,
                ftst: false,
            }),
            Err(AdapterError::MissingKey { .. }) => {
                self.transport.info(Key::ModeLower(fan))?;
                Ok(Route::PerFan {
                    lowercase: true,
                    ftst: false,
                })
            }
            Err(error) => Err(error),
        }
    }
    fn mode_value(&mut self, key: Key) -> Result<Mode, AdapterError> {
        let (metadata, value) = self.number(key)?;
        if metadata.kind != *b"ui8 " || metadata.size != 1 {
            return Err(AdapterError::malformed(key, "mode is not ui8/1"));
        }
        if let Some(profile) = self.profile {
            let fan = match key {
                Key::ModeUpper(fan) | Key::ModeLower(fan) => fan,
                _ => return Err(AdapterError::malformed(key, "not a direct mode key")),
            };
            if metadata != profile.fan(fan)?.mode_metadata {
                return Err(AdapterError::MetadataChanged { key: key.name()? });
            }
        }
        Ok(match value as u8 {
            0 => Mode::Automatic,
            1 => Mode::Manual,
            _ => Mode::Unknown,
        })
    }
    fn state(&mut self, fan: FanId) -> Result<HardwareState, AdapterError> {
        self.require_fan(fan)?;
        let route = self.route(fan)?;
        let mode = match route {
            Route::SharedMask => {
                let (metadata, mask) = self.number(Key::Mask)?;
                if self
                    .profile
                    .is_some_and(|profile| profile.mask_metadata != Some(metadata))
                {
                    return Err(AdapterError::MetadataChanged {
                        key: Key::Mask.name()?,
                    });
                }
                let inferred = if mask as u16 & (1 << fan.0) == 0 {
                    Mode::Automatic
                } else {
                    Mode::Manual
                };
                // Without a hardware profile, visible conflicting sources are not silently resolved.
                let mut conflict = false;
                if self.profile.is_none() {
                    for key in [Key::ModeUpper(fan), Key::ModeLower(fan)] {
                        match self.mode_value(key) {
                            Ok(other) if other != inferred => conflict = true,
                            Ok(_) | Err(AdapterError::MissingKey { .. }) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
                if conflict {
                    Mode::Unknown
                } else {
                    inferred
                }
            }
            Route::PerFan { ftst, .. } => {
                let mut mode = self.mode_value(direct_key(route, fan))?;
                if ftst {
                    // A direct automatic byte alone cannot establish restoration:
                    // Ftst must agree with every fan's current direct mode.
                    let profile = self
                        .profile
                        .expect("Ftst route requires an admitted profile");
                    if self.expected(Key::Count, profile.count_metadata)?
                        != profile.fans.len() as f64
                    {
                        return Err(AdapterError::malformed(
                            Key::Count,
                            "fan inventory differs from Ftst profile",
                        ));
                    }
                    let mut any_manual = false;
                    for spec in profile.fans {
                        let key = direct_key(route, spec.fan);
                        match self.expected(key, spec.mode_metadata)? {
                            0.0 => {
                                if spec.fan == fan {
                                    mode = Mode::Automatic;
                                }
                            }
                            1.0 => {
                                any_manual = true;
                                if spec.fan == fan {
                                    mode = Mode::Manual;
                                }
                            }
                            _ => {
                                return Err(AdapterError::malformed(
                                    key,
                                    "unknown mode while checking Ftst",
                                ))
                            }
                        }
                    }
                    let gate = self.expected(
                        Key::Gate,
                        profile.gate_metadata.expect("validated gate profile"),
                    )?;
                    if gate != f64::from(u8::from(any_manual)) {
                        return Err(AdapterError::ReadbackMismatch {
                            key: Key::Gate.name()?,
                        });
                    }
                }
                mode
            }
        };
        let (metadata, target) = self.number(Key::Target(fan))?;
        if let Some(profile) = self.profile {
            if metadata != profile.fan(fan)?.target_metadata {
                return Err(AdapterError::MetadataChanged {
                    key: Key::Target(fan).name()?,
                });
            }
        }
        if metadata.target_encoding() == TargetEncoding::Unknown
            || !(0.0..=30000.0).contains(&target)
        {
            return Err(AdapterError::malformed(
                Key::Target(fan),
                "invalid target type/range",
            ));
        }
        Ok(HardwareState {
            mode,
            target_rpm: Some(target),
        })
    }
    fn control_admission(&mut self) -> ControlAdmission {
        if self.profile.is_none() {
            return ControlAdmission::NoReviewedProfile;
        }
        let Ok(fans) = self.inventory() else {
            return ControlAdmission::ProfileValidationFailed;
        };
        if fans.is_empty()
            || fans
                .into_iter()
                .any(|fan| self.validated(fan).is_err() || self.state(fan).is_err())
        {
            ControlAdmission::ProfileValidationFailed
        } else {
            ControlAdmission::Admitted
        }
    }

    fn validated(&mut self, fan: FanId) -> Result<&'static Profile, AdapterError> {
        // Refuse BEFORE any possible write. Read-only discoveries never construct profiles.
        let profile = self.profile.ok_or_else(|| AdapterError::Denied {
            reason: profile::refusal(&self.identity),
        })?;
        self.check_identity()?;
        profile.validate(&self.identity)?;
        let inventory = self.inventory()?;
        if inventory.len() != profile.fans.len() {
            return Err(AdapterError::malformed(
                Key::Count,
                "fan inventory changed from hardware profile",
            ));
        }
        profile.fan(fan)?;
        self.expected(Key::Count, profile.count_metadata)?;
        for spec in profile.fans {
            for (key, metadata) in [
                (Key::Minimum(spec.fan), spec.min_metadata),
                (Key::Maximum(spec.fan), spec.max_metadata),
                (Key::Target(spec.fan), spec.target_metadata),
            ] {
                let value = self.expected(key, metadata)?;
                if (key == Key::Minimum(spec.fan) && value != spec.min)
                    || (key == Key::Maximum(spec.fan) && value != spec.max)
                {
                    return Err(AdapterError::malformed(
                        key,
                        "range differs from hardware profile",
                    ));
                }
                if !(0.0..=30000.0).contains(&value) {
                    return Err(AdapterError::malformed(key, "invalid RPM range"));
                }
            }
            if let Route::PerFan { .. } = profile.route {
                let value =
                    self.expected(direct_key(profile.route, spec.fan), spec.mode_metadata)?;
                if value != 0.0 && value != 1.0 {
                    return Err(AdapterError::malformed(
                        direct_key(profile.route, spec.fan),
                        "unknown mode semantics",
                    ));
                }
            }
        }
        match profile.route {
            Route::SharedMask => {
                self.expected(
                    Key::Mask,
                    profile.mask_metadata.expect("validated mask profile"),
                )?;
            }
            Route::PerFan { ftst: true, .. } => {
                let value = self.expected(
                    Key::Gate,
                    profile.gate_metadata.expect("validated gate profile"),
                )?;
                if value != 0.0 && value != 1.0 {
                    return Err(AdapterError::malformed(Key::Gate, "unknown Ftst semantics"));
                }
            }
            Route::PerFan { ftst: false, .. } => {}
        }
        Ok(profile)
    }
    fn caps(&mut self, fan: FanId) -> Result<Capabilities, AdapterError> {
        self.require_fan(fan)?;
        let (metadata, min) = self.number(Key::Minimum(fan))?;
        let (max_metadata, max) = self.number(Key::Maximum(fan))?;
        let target_metadata = self.transport.info(Key::Target(fan))?;
        if min < 0.0
            || max < min
            || max > 30000.0
            || metadata.target_encoding() == TargetEncoding::Unknown
            || !max_metadata.same_encoding(metadata)
            || !target_metadata.same_encoding(metadata)
        {
            return Err(AdapterError::malformed(
                Key::Target(fan),
                "inconsistent RPM metadata/range",
            ));
        }
        let verified = if self.profile.is_some() {
            self.validated(fan)?;
            true
        } else {
            false
        };
        let mode_control = match self
            .profile
            .filter(|_| verified)
            .map(|profile| profile.route)
        {
            Some(Route::SharedMask) => ModeControl::VerifiedSharedMask,
            Some(Route::PerFan { .. }) => ModeControl::PerFan,
            None => ModeControl::Unknown,
        };
        Ok(Capabilities {
            fan,
            validated: verified,
            model: if verified {
                ModelSupport::Verified
            } else {
                ModelSupport::Unknown
            },
            encoding: metadata.target_encoding(),
            mode_control,
            min_rpm: min,
            max_rpm: max,
            step_rpm: self.profile.and_then(|p| p.fan(fan).ok()).map_or(
                if metadata.target_encoding() == TargetEncoding::Fpe2 {
                    0.25
                } else {
                    1.0
                },
                |s| s.step,
            ),
            stages_target_while_automatic: verified,
        })
    }
    fn commit(
        &mut self,
        profile: &'static Profile,
        key: Key,
        metadata: Metadata,
        bytes: &[u8],
    ) -> Result<(), AdapterError> {
        let admission = Admission::for_write(profile, &self.identity, key, metadata)?;
        self.transport.write(key, metadata, bytes, &admission)
    }
    fn target(&mut self, fan: FanId, rpm: f64) -> Result<(), AdapterError> {
        let profile = self.validated(fan)?;
        let spec = profile.fan(fan)?;
        if !rpm.is_finite()
            || rpm < spec.min
            || rpm > spec.max
            || rpm / spec.step != (rpm / spec.step).trunc()
            || rpm == 0.0 && !spec.allow_zero
        {
            return Err(AdapterError::malformed(
                Key::Target(fan),
                "target outside admitted RPM grid/range",
            ));
        }
        let bytes = encode_target(spec.target_metadata, rpm)?;
        if self.state(fan)?.mode != Mode::Automatic {
            return Err(AdapterError::Denied {
                reason: "Target staging requires confirmed automatic mode".into(),
            });
        }
        self.commit(profile, Key::Target(fan), spec.target_metadata, &bytes)?;
        let state = self.state(fan)?;
        if state.mode != Mode::Automatic || state.target_rpm != Some(rpm) {
            return Err(AdapterError::ReadbackMismatch {
                key: Key::Target(fan).name()?,
            });
        }
        Ok(())
    }
    fn set_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), AdapterError> {
        let profile = self.validated(fan)?;
        let desired = if mode == WriteMode::Manual { 1u8 } else { 0u8 };
        // Restoration must be able to repair a valid-but-inconsistent Ftst.
        // `validated` above still checks identity, inventory, metadata, ranges
        // and 0/1 mode/gate values. Only manual activation requires a fully
        // consistent state before it writes; automatic must repair then verify.
        let before = if desired == 1 {
            Some(self.state(fan)?)
        } else {
            None
        };
        match profile.route {
            Route::SharedMask => {
                let metadata = profile.mask_metadata.expect("validated mask profile");
                let mask = self.expected(Key::Mask, metadata)? as u16;
                let next = if desired == 1 {
                    mask | (1 << fan.0)
                } else {
                    mask & !(1 << fan.0)
                };
                self.commit(profile, Key::Mask, metadata, &next.to_be_bytes())?;
                if self.expected(Key::Mask, metadata)? as u16 != next {
                    return Err(AdapterError::ReadbackMismatch {
                        key: Key::Mask.name()?,
                    });
                }
            }
            Route::PerFan { ftst, .. } => {
                let key = direct_key(profile.route, fan);
                let metadata = profile.fan(fan)?.mode_metadata;
                self.commit(profile, key, metadata, &[desired])?;
                if self.expected(key, metadata)? != desired as f64 {
                    return Err(AdapterError::ReadbackMismatch { key: key.name()? });
                }
                if ftst {
                    let mut any_manual = false;
                    for other in profile.fans {
                        let key = direct_key(profile.route, other.fan);
                        match self.expected(key, other.mode_metadata)? {
                            0.0 => {}
                            1.0 => any_manual = true,
                            _ => {
                                return Err(AdapterError::malformed(
                                    key,
                                    "unknown mode after write",
                                ))
                            }
                        }
                    }
                    let gate = u8::from(any_manual);
                    let metadata = profile.gate_metadata.expect("validated gate profile");
                    self.commit(profile, Key::Gate, metadata, &[gate])?;
                    if self.expected(Key::Gate, metadata)? != gate as f64 {
                        return Err(AdapterError::ReadbackMismatch {
                            key: Key::Gate.name()?,
                        });
                    }
                }
            }
        }
        let after = self.state(fan)?;
        let expected = if desired == 1 {
            Mode::Manual
        } else {
            Mode::Automatic
        };
        if after.mode != expected {
            return Err(AdapterError::ReadbackMismatch { key: "mode".into() });
        }
        if before.is_some_and(|before| after.target_rpm != before.target_rpm) {
            return Err(AdapterError::ReadbackMismatch {
                key: Key::Target(fan).name()?,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedKey {
    pub key: String,
    pub metadata: Option<Metadata>,
    pub encoding: Option<String>,
    pub value: Option<f64>,
    pub error: Option<AdapterError>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FanProbe {
    pub fan: u16,
    pub keys: Vec<ObservedKey>,
    pub mode: String,
    pub target_rpm: Option<f64>,
    pub state_error: Option<AdapterError>,
    pub capabilities: Option<CapabilityObservation>,
    pub capability_error: Option<AdapterError>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityObservation {
    pub validated: bool,
    pub model_support: String,
    pub target_encoding: String,
    pub mode_control: String,
    pub min_rpm: f64,
    pub max_rpm: f64,
    pub step_rpm: f64,
    pub stages_target_while_automatic: bool,
}
impl From<Capabilities> for CapabilityObservation {
    fn from(caps: Capabilities) -> Self {
        Self {
            validated: caps.validated,
            model_support: format!("{:?}", caps.model),
            target_encoding: format!("{:?}", caps.encoding),
            mode_control: format!("{:?}", caps.mode_control),
            min_rpm: caps.min_rpm,
            max_rpm: caps.max_rpm,
            step_rpm: caps.step_rpm,
            stages_target_while_automatic: caps.stages_target_while_automatic,
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub identity: Identity,
    pub protocol_implemented: bool,
    pub hardware_profile_verified: bool,
    pub write_refusal: Vec<String>,
    pub globals: Vec<ObservedKey>,
    pub fans: Vec<FanProbe>,
    pub inventory_error: Option<AdapterError>,
    pub hardware_write_attempts: u64,
}

/// Opens only our AppleSMC user client. No original vendor process/helper is used.
/// Calls to Actuator writes remain denied until an internal production profile exists.
pub struct SmcAdapter {
    inner: Inner<RealTransport>,
    last_error: Option<AdapterError>,
}
impl SmcAdapter {
    /// Select only a compiled, reviewed profile from the real hardware identity.
    /// This accepts no caller-supplied capability or validation assertion.
    pub fn open() -> Result<Self, AdapterError> {
        Ok(Self {
            inner: Inner::new(RealTransport::open()?)?,
            last_error: None,
        })
    }
    pub fn open_readonly() -> Result<Self, AdapterError> {
        let mut adapter = Self::open()?;
        // Irreversibly suppress admission for this instance, even if future
        // releases add a production profile. There is no public promotion API.
        adapter.inner.profile = None;
        Ok(adapter)
    }
    /// Irreversibly remove this instance's write profile. This only reduces
    /// authority; no public API can restore a profile to the returned adapter.
    pub fn into_readonly(mut self) -> Self {
        self.inner = self.inner.into_readonly();
        self
    }
    /// Read the full inventory against the internally selected profile, without
    /// performing writes or granting the caller any capability to select a profile.
    pub fn control_admission(&mut self) -> ControlAdmission {
        self.inner.control_admission()
    }
    pub fn last_error(&self) -> Option<&AdapterError> {
        self.last_error.as_ref()
    }
    fn result<R>(&mut self, result: Result<R, AdapterError>) -> Result<R, ActuatorError> {
        match result {
            Ok(value) => {
                self.last_error = None;
                Ok(value)
            }
            Err(error) => {
                let mapped = error.core();
                self.last_error = Some(error);
                Err(mapped)
            }
        }
    }
    pub fn probe(&mut self) -> ProbeReport {
        let hardware_write_attempts = self.inner.transport.write_attempts();
        self.inner.probe(hardware_write_attempts)
    }
}
impl<T: Transport> Inner<T> {
    fn probe(&mut self, hardware_write_attempts: u64) -> ProbeReport {
        fn observe<T: Transport>(inner: &mut Inner<T>, key: Key) -> ObservedKey {
            let name = key.name().expect("bounded probe key");
            match inner.transport.info(key) {
                Ok(metadata) => {
                    let value = inner
                        .transport
                        .read(key, metadata)
                        .and_then(|bytes| decode(key, metadata, &bytes));
                    ObservedKey {
                        key: name,
                        encoding: Some(String::from_utf8_lossy(&metadata.kind).into()),
                        metadata: Some(metadata),
                        value: value.as_ref().ok().copied(),
                        error: value.err(),
                    }
                }
                Err(error) => ObservedKey {
                    key: name,
                    metadata: None,
                    encoding: None,
                    value: None,
                    error: Some(error),
                },
            }
        }
        let inventory = self.inventory();
        let mut report = ProbeReport {
            identity: self.identity.clone(),
            protocol_implemented: true,
            hardware_profile_verified: false,
            write_refusal: vec![profile::refusal(&self.identity)],
            globals: [Key::Count, Key::Mask, Key::Gate]
                .into_iter()
                .map(|key| observe(self, key))
                .collect(),
            fans: Vec::new(),
            inventory_error: inventory.as_ref().err().cloned(),
            hardware_write_attempts,
        };
        if let Ok(fans) = inventory {
            for fan in fans {
                let keys = [
                    Key::Current(fan),
                    Key::Minimum(fan),
                    Key::Maximum(fan),
                    Key::Target(fan),
                    Key::ModeUpper(fan),
                    Key::ModeLower(fan),
                ]
                .into_iter()
                .map(|key| observe(self, key))
                .collect();
                let state = self.state(fan);
                let capabilities = self.caps(fan);
                report.fans.push(FanProbe {
                    fan: fan.0,
                    keys,
                    mode: match state.as_ref().map(|state| state.mode) {
                        Ok(Mode::Automatic) => "automatic",
                        Ok(Mode::Manual) => "manual",
                        _ => "unknown",
                    }
                    .into(),
                    target_rpm: state.as_ref().ok().and_then(|state| state.target_rpm),
                    state_error: state.err(),
                    capabilities: capabilities.as_ref().ok().cloned().map(Into::into),
                    capability_error: capabilities.err(),
                });
            }
        }
        // A later reviewed profile would still require all fresh validations here.
        if self.profile.is_some() && !report.fans.is_empty() {
            let valid = report
                .fans
                .iter()
                .all(|fan| self.validated(FanId(fan.fan)).is_ok());
            if valid {
                report.hardware_profile_verified = true;
                report.write_refusal.clear();
            }
        }
        report
    }
}
impl Actuator for SmcAdapter {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        let result = self.inner.inventory();
        self.result(result)
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        let result = self.inner.caps(fan);
        self.result(result)
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        let result = self.inner.state(fan);
        self.result(result)
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        let result = self.inner.target(fan, rpm);
        self.result(result)
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        let result = self.inner.set_mode(fan, mode);
        self.result(result)
    }
}

#[cfg(test)]
mod tests;
