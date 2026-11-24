use super::*;
use macs_fan_control_core::{
    Command, Engine, LeaseToken, Owner, Phase, Response, TickReason, TokenError, TokenSource,
};
use profile::{FanSpec, RecoveryEvidence};
use std::collections::HashMap;
use std::{cell::RefCell, rc::Rc};

const FPE: Metadata = Metadata {
    size: 2,
    kind: *b"fpe2",
    attributes: 0,
};
const FLOAT: Metadata = Metadata {
    size: 4,
    kind: *b"flt ",
    attributes: 0,
};
const fn evidence() -> RecoveryEvidence {
    RecoveryEvidence {
        artifact: "MOCK transport evidence only; never real admission",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        automatic_restore: true,
        staged_target: true,
        disconnect_watchdog: true,
        helper_crash: true,
        sleep_wake: true,
        competing_controller: true,
    }
}
static FPE_FANS: [FanSpec; 2] = [
    FanSpec {
        fan: FanId(0),
        target_metadata: FPE,
        min_metadata: FPE,
        max_metadata: FPE,
        mode_metadata: Metadata::UI8,
        min: 1000.0,
        max: 6000.0,
        step: 0.25,
        allow_zero: false,
    },
    FanSpec {
        fan: FanId(1),
        target_metadata: FPE,
        min_metadata: FPE,
        max_metadata: FPE,
        mode_metadata: Metadata::UI8,
        min: 1000.0,
        max: 6000.0,
        step: 0.25,
        allow_zero: false,
    },
];
static FLOAT_FANS: [FanSpec; 2] = [
    FanSpec {
        fan: FanId(0),
        target_metadata: FLOAT,
        min_metadata: FLOAT,
        max_metadata: FLOAT,
        mode_metadata: Metadata::UI8,
        min: 1000.0,
        max: 6000.0,
        step: 0.25,
        allow_zero: false,
    },
    FanSpec {
        fan: FanId(1),
        target_metadata: FLOAT,
        min_metadata: FLOAT,
        max_metadata: FLOAT,
        mode_metadata: Metadata::UI8,
        min: 1000.0,
        max: 6000.0,
        step: 0.25,
        allow_zero: false,
    },
];
static UPPER: Profile = Profile {
    model: "MockMac",
    architecture: "mock",
    route: Route::PerFan {
        lowercase: false,
        ftst: true,
    },
    count_metadata: Metadata::UI8,
    mask_metadata: Some(Metadata::UI16),
    gate_metadata: Some(Metadata::UI8),
    fans: &FPE_FANS,
    evidence: evidence(),
};
static LOWER: Profile = Profile {
    model: "MockMac",
    architecture: "mock",
    route: Route::PerFan {
        lowercase: true,
        ftst: true,
    },
    count_metadata: Metadata::UI8,
    mask_metadata: Some(Metadata::UI16),
    gate_metadata: Some(Metadata::UI8),
    fans: &FLOAT_FANS,
    evidence: evidence(),
};
static MASK: Profile = Profile {
    model: "MockMac",
    architecture: "mock",
    route: Route::SharedMask,
    count_metadata: Metadata::UI8,
    mask_metadata: Some(Metadata::UI16),
    gate_metadata: Some(Metadata::UI8),
    fans: &FPE_FANS,
    evidence: evidence(),
};

const fn attribute_spec(fan: u16) -> FanSpec {
    FanSpec {
        fan: FanId(fan),
        min_metadata: Metadata {
            attributes: 132,
            ..FLOAT
        },
        max_metadata: Metadata {
            attributes: 133,
            ..FLOAT
        },
        target_metadata: Metadata {
            attributes: 212,
            ..FLOAT
        },
        mode_metadata: Metadata {
            attributes: 208,
            ..Metadata::UI8
        },
        min: 1000.0,
        max: 6000.0,
        step: 0.25,
        allow_zero: false,
    }
}
static ATTRIBUTE_FANS: [FanSpec; 2] = [attribute_spec(0), attribute_spec(1)];
static ATTRIBUTE_PROFILE: Profile = Profile {
    model: "MockMac",
    architecture: "mock",
    route: Route::PerFan {
        lowercase: false,
        ftst: true,
    },
    count_metadata: Metadata {
        attributes: 128,
        ..Metadata::UI8
    },
    mask_metadata: None,
    gate_metadata: Some(Metadata {
        attributes: 208,
        ..Metadata::UI8
    }),
    fans: &ATTRIBUTE_FANS,
    evidence: evidence(),
};

struct Mock {
    identity: Identity,
    keys: HashMap<Key, (Metadata, Vec<u8>)>,
    events: Vec<String>,
    ignore: Option<Key>,
    write_error: Option<AdapterError>,
    target_after_manual: Option<f64>,
    mode_after_target: bool,
}
impl Mock {
    fn new(profile: &Profile) -> Self {
        let mut result = Self {
            identity: Identity {
                model: "MockMac".into(),
                architecture: "mock".into(),
            },
            keys: HashMap::new(),
            events: Vec::new(),
            ignore: None,
            write_error: None,
            target_after_manual: None,
            mode_after_target: false,
        };
        result
            .keys
            .insert(Key::Count, (profile.count_metadata, vec![2]));
        result.keys.insert(
            Key::Mask,
            (profile.mask_metadata.unwrap_or(Metadata::UI16), vec![0, 0]),
        );
        result.keys.insert(
            Key::Gate,
            (profile.gate_metadata.unwrap_or(Metadata::UI8), vec![0]),
        );
        for spec in profile.fans {
            for (key, value, metadata) in [
                (Key::Minimum(spec.fan), spec.min, spec.min_metadata),
                (Key::Maximum(spec.fan), spec.max, spec.max_metadata),
                (Key::Target(spec.fan), 2000.0, spec.target_metadata),
                (Key::Current(spec.fan), 2000.0, spec.target_metadata),
            ] {
                result
                    .keys
                    .insert(key, (metadata, encode_target(metadata, value).unwrap()));
            }
            result.keys.insert(
                direct_key(profile.route, spec.fan),
                (spec.mode_metadata, vec![0]),
            );
        }
        result
    }
    fn writes(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|entry| entry.starts_with("W:"))
            .cloned()
            .collect()
    }
    fn byte(&self, key: Key) -> u8 {
        self.keys[&key].1[0]
    }
}
impl Transport for Mock {
    fn identity(&mut self) -> Result<Identity, AdapterError> {
        self.events.push("identity".into());
        Ok(self.identity.clone())
    }
    fn info(&mut self, key: Key) -> Result<Metadata, AdapterError> {
        self.events.push(format!("I:{}", key.name()?));
        self.keys
            .get(&key)
            .map(|(metadata, _)| *metadata)
            .ok_or(AdapterError::MissingKey { key: key.name()? })
    }
    fn read(&mut self, key: Key, metadata: Metadata) -> Result<Vec<u8>, AdapterError> {
        self.events.push(format!("R:{}", key.name()?));
        let (actual, bytes) = self
            .keys
            .get(&key)
            .ok_or(AdapterError::MissingKey { key: key.name()? })?;
        if *actual != metadata {
            return Err(AdapterError::MetadataChanged { key: key.name()? });
        }
        Ok(bytes.clone())
    }
    fn write(
        &mut self,
        key: Key,
        metadata: Metadata,
        bytes: &[u8],
        admission: &Admission,
    ) -> Result<(), AdapterError> {
        admission.verify(&self.identity, key, metadata)?;
        if self.info(key)? != metadata || bytes.len() != metadata.size as usize {
            return Err(AdapterError::MetadataChanged { key: key.name()? });
        }
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.events.push(format!("W:{}:{hex}", key.name()?));
        if let Some(error) = self.write_error.clone() {
            return Err(error);
        }
        if self.ignore != Some(key) {
            self.keys.insert(key, (metadata, bytes.to_vec()));
        }
        if let Key::Target(fan) = key {
            if self.mode_after_target {
                self.keys
                    .insert(Key::ModeUpper(fan), (Metadata::UI8, vec![1]));
            }
        }
        if matches!(key, Key::ModeUpper(_) | Key::ModeLower(_)) && bytes == [1] {
            if let Some(target) = self.target_after_manual {
                let fan = match key {
                    Key::ModeUpper(fan) | Key::ModeLower(fan) => fan,
                    _ => unreachable!(),
                };
                let metadata = self.keys[&Key::Target(fan)].0;
                self.keys.insert(
                    Key::Target(fan),
                    (metadata, encode_target(metadata, target)?),
                );
            }
        }
        Ok(())
    }
}
fn admitted(profile: &'static Profile) -> Inner<Mock> {
    let mut inner = Inner::new(Mock::new(profile)).unwrap();
    // Test fixture admission is private to this module and the Mock transport.
    assert!(inner.profile.is_none());
    inner.profile = Some(profile);
    inner
}

#[test]
fn exact_target_encodings_and_rejections() {
    assert_eq!(encode_target(FPE, 2000.25).unwrap(), [0x1f, 0x41]);
    assert_eq!(encode_target(FPE, 16383.75).unwrap(), [0xff, 0xff]);
    assert_eq!(
        encode_target(FLOAT, 2000.25).unwrap(),
        2000.25f32.to_le_bytes()
    );
    for invalid in [f64::NAN, f64::INFINITY, -1.0, 16384.0, 2000.1] {
        assert!(encode_target(FPE, invalid).is_err());
    }
    for invalid in [f64::NAN, f64::INFINITY, -1.0, 1.1, f64::MAX] {
        assert!(encode_target(FLOAT, invalid).is_err());
    }
    assert!(encode_target(Metadata { size: 4, ..FPE }, 2000.0).is_err());
    assert!(decode(Key::Target(FanId(0)), FLOAT, &f32::NAN.to_le_bytes()).is_err());
    assert!(decode(Key::Target(FanId(0)), FPE, &[0]).is_err());
}

#[test]
fn complete_uppercase_stage_manual_and_automatic_sequence() {
    let mut inner = admitted(&UPPER);
    inner.target(FanId(0), 2000.25).unwrap();
    inner.set_mode(FanId(0), WriteMode::Manual).unwrap();
    inner.set_mode(FanId(0), WriteMode::Automatic).unwrap();
    assert_eq!(
        inner.transport.writes(),
        [
            "W:F0Tg:1f41",
            "W:F0Md:01",
            "W:Ftst:01",
            "W:F0Md:00",
            "W:Ftst:00"
        ]
    );
    for (index, event) in inner.transport.events.iter().enumerate() {
        if let Some(write) = event.strip_prefix("W:") {
            let key = write.split(':').next().unwrap();
            assert_eq!(inner.transport.events[index - 1], format!("I:{key}"));
            assert!(inner.transport.events[index + 1..]
                .iter()
                .any(|event| event == &format!("R:{key}")));
        }
    }
}

#[test]
fn lowercase_float_profile_uses_only_admitted_keys() {
    let mut inner = admitted(&LOWER);
    inner.target(FanId(1), 2000.25).unwrap();
    inner.set_mode(FanId(1), WriteMode::Manual).unwrap();
    let expected = format!(
        "W:F1Tg:{}",
        2000.25f32
            .to_le_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(
        inner.transport.writes(),
        [expected, "W:F1md:01".into(), "W:Ftst:01".into()]
    );
    assert!(!inner
        .transport
        .events
        .iter()
        .any(|event| event.contains("F1Md")));
}

#[test]
fn mask_rmw_preserves_all_other_bits_including_unenumerated_bits() {
    let mut inner = admitted(&MASK);
    inner.transport.keys.insert(
        Key::Mask,
        (Metadata::UI16, 0x8002u16.to_be_bytes().to_vec()),
    );
    inner.set_mode(FanId(0), WriteMode::Manual).unwrap();
    inner.set_mode(FanId(0), WriteMode::Automatic).unwrap();
    assert_eq!(inner.transport.writes(), ["W:FS! :8003", "W:FS! :8002"]);
    assert_eq!(inner.transport.keys[&Key::Mask].1, 0x8002u16.to_be_bytes());
}

#[test]
fn ftst_tracks_any_manual_not_just_the_fan_being_released() {
    let mut inner = admitted(&UPPER);
    inner
        .transport
        .keys
        .insert(Key::ModeUpper(FanId(1)), (Metadata::UI8, vec![1]));
    inner
        .transport
        .keys
        .insert(Key::Gate, (Metadata::UI8, vec![1]));
    assert_eq!(inner.state(FanId(0)).unwrap().mode, Mode::Automatic);
    inner.set_mode(FanId(0), WriteMode::Manual).unwrap();
    inner.set_mode(FanId(0), WriteMode::Automatic).unwrap();
    assert_eq!(inner.transport.byte(Key::Gate), 1);
    assert_eq!(inner.transport.byte(Key::ModeUpper(FanId(1))), 1);
    inner.set_mode(FanId(1), WriteMode::Automatic).unwrap();
    assert_eq!(inner.transport.byte(Key::Gate), 0);
    assert_eq!(
        inner.transport.writes(),
        [
            "W:F0Md:01",
            "W:Ftst:01",
            "W:F0Md:00",
            "W:Ftst:01",
            "W:F1Md:00",
            "W:Ftst:00"
        ]
    );
}

#[test]
fn metadata_type_size_attributes_ranges_and_inventory_drift_refuse_before_writes() {
    for changed in [
        Metadata { size: 4, ..FPE },
        FLOAT,
        Metadata {
            attributes: 1,
            ..FPE
        },
    ] {
        let mut inner = admitted(&UPPER);
        inner
            .transport
            .keys
            .get_mut(&Key::Target(FanId(0)))
            .unwrap()
            .0 = changed;
        assert!(matches!(
            inner.target(FanId(0), 2000.0),
            Err(AdapterError::MetadataChanged { .. })
        ));
        assert!(inner.transport.writes().is_empty());
    }
    let mut inner = admitted(&UPPER);
    inner.transport.keys.insert(
        Key::Maximum(FanId(0)),
        (FPE, encode_target(FPE, 5999.0).unwrap()),
    );
    assert!(inner.target(FanId(0), 2000.0).is_err());
    assert!(inner.transport.writes().is_empty());
    let mut inner = admitted(&UPPER);
    inner
        .transport
        .keys
        .insert(Key::Count, (Metadata::UI8, vec![1]));
    assert!(inner.set_mode(FanId(0), WriteMode::Manual).is_err());
    assert!(inner.transport.writes().is_empty());
    let mut inner = admitted(&UPPER);
    inner.transport.identity.model = "ChangedMac".into();
    assert_eq!(
        inner.target(FanId(0), 2000.0),
        Err(AdapterError::IdentityChanged)
    );
    assert!(inner.transport.writes().is_empty());
}

#[test]
fn write_acknowledgement_without_matching_readback_is_an_error() {
    let mut inner = admitted(&UPPER);
    inner.transport.ignore = Some(Key::Target(FanId(0)));
    assert!(
        matches!(inner.target(FanId(0),2000.25),Err(AdapterError::ReadbackMismatch {key}) if key=="F0Tg")
    );
    let mut inner = admitted(&UPPER);
    inner.transport.ignore = Some(Key::ModeUpper(FanId(0)));
    assert!(
        matches!(inner.set_mode(FanId(0),WriteMode::Manual),Err(AdapterError::ReadbackMismatch {key}) if key=="F0Md")
    );
    assert_eq!(inner.transport.writes(), ["W:F0Md:01"]);
    let mut inner = admitted(&UPPER);
    inner.transport.ignore = Some(Key::Gate);
    assert!(
        matches!(inner.set_mode(FanId(0),WriteMode::Manual),Err(AdapterError::ReadbackMismatch {key}) if key=="Ftst")
    );
}

#[test]
fn readback_detects_mode_changed_during_target_stage_and_target_changed_during_mode_write() {
    let mut inner = admitted(&UPPER);
    inner.transport.mode_after_target = true;
    assert!(matches!(
        inner.target(FanId(0), 2000.25),
        Err(AdapterError::ReadbackMismatch { .. })
    ));
    let mut inner = admitted(&UPPER);
    inner.transport.target_after_manual = Some(2100.0);
    assert!(
        matches!(inner.set_mode(FanId(0),WriteMode::Manual),Err(AdapterError::ReadbackMismatch {key}) if key=="F0Tg")
    );
}

#[test]
fn transport_errors_preserve_codes_and_core_classification() {
    for error in [
        AdapterError::IoKit { code: 0xe00002bc },
        AdapterError::Smc {
            key: "F0Tg".into(),
            code: 0x85,
        },
        AdapterError::Timeout,
    ] {
        let mut inner = admitted(&UPPER);
        inner.transport.write_error = Some(error.clone());
        assert_eq!(inner.target(FanId(0), 2000.25), Err(error.clone()));
        assert_eq!(
            error.core(),
            if error == AdapterError::Timeout {
                ActuatorError::Timeout
            } else {
                ActuatorError::Io
            }
        );
        assert_eq!(inner.transport.writes().len(), 1);
    }
}

#[test]
fn production_discovery_never_claims_validation_or_allows_writes() {
    let mut inner = Inner::new(Mock::new(&UPPER)).unwrap();
    let caps = inner.caps(FanId(0)).unwrap();
    assert!(!caps.validated);
    assert_eq!(caps.model, ModelSupport::Unknown);
    assert_eq!(caps.mode_control, ModeControl::Unknown);
    assert!(!caps.stages_target_while_automatic);
    assert!(matches!(
        inner.target(FanId(0), 2000.0),
        Err(AdapterError::Denied { .. })
    ));
    assert!(matches!(
        inner.set_mode(FanId(0), WriteMode::Automatic),
        Err(AdapterError::Denied { .. })
    ));
    assert!(inner.transport.writes().is_empty());
    let identity = Identity {
        model: "Mac16,8".into(),
        architecture: "aarch64".into(),
    };
    assert!(profile::select(&identity).is_none());
    assert!(profile::refusal(&identity).contains("not-working"));
    let admission =
        Admission::for_write(&UPPER, &inner.identity, Key::Target(FanId(0)), FPE).unwrap();
    assert!(admission.verify_real_registry().is_err());
    assert!(admission
        .verify(&inner.identity, Key::Minimum(FanId(0)), FPE)
        .is_err());
    assert!(Admission::for_write(&UPPER, &inner.identity, Key::Gate, Metadata::UI16).is_err());
}

#[test]
fn unknown_raw_mode_three_and_conflicting_sources_stay_unknown() {
    let mut mock = Mock::new(&UPPER);
    mock.keys.remove(&Key::Mask);
    mock.keys
        .insert(Key::ModeUpper(FanId(0)), (Metadata::UI8, vec![3]));
    let mut inner = Inner::new(mock).unwrap();
    assert_eq!(inner.state(FanId(0)).unwrap().mode, Mode::Unknown);
    assert!(inner.transport.writes().is_empty());
    inner
        .transport
        .keys
        .insert(Key::Mask, (Metadata::UI16, vec![0, 0]));
    inner
        .transport
        .keys
        .insert(Key::ModeUpper(FanId(0)), (Metadata::UI8, vec![1]));
    assert_eq!(inner.state(FanId(0)).unwrap().mode, Mode::Unknown);
}

#[test]
fn invalid_fan_rpm_grid_mode_and_manual_target_staging_cannot_write() {
    assert!(Key::Target(FanId(16)).name().is_err());
    for rpm in [0.0, 999.75, 6000.25, 2000.1, f64::NAN, f64::INFINITY] {
        let mut inner = admitted(&UPPER);
        assert!(inner.target(FanId(0), rpm).is_err());
        assert!(inner.transport.writes().is_empty());
    }
    let mut inner = admitted(&UPPER);
    inner
        .transport
        .keys
        .insert(Key::ModeUpper(FanId(0)), (Metadata::UI8, vec![1]));
    assert!(inner.target(FanId(0), 2000.0).is_err());
    assert!(inner.transport.writes().is_empty());
    inner
        .transport
        .keys
        .insert(Key::ModeUpper(FanId(0)), (Metadata::UI8, vec![3]));
    assert!(inner.set_mode(FanId(0), WriteMode::Automatic).is_err());
    assert!(inner.transport.writes().is_empty());
}

#[test]
fn distinct_per_key_attributes_are_accepted_and_later_drift_is_rejected() {
    let mut inner = admitted(&ATTRIBUTE_PROFILE);
    inner.target(FanId(0), 2000.25).unwrap();
    inner.set_mode(FanId(0), WriteMode::Manual).unwrap();
    inner.set_mode(FanId(0), WriteMode::Automatic).unwrap();
    assert_eq!(inner.transport.writes().len(), 5);
    inner
        .transport
        .keys
        .get_mut(&Key::ModeUpper(FanId(0)))
        .unwrap()
        .0
        .attributes = 0;
    let count = inner.transport.writes().len();
    assert!(matches!(
        inner.target(FanId(0), 2000.0),
        Err(AdapterError::MetadataChanged { .. })
    ));
    assert_eq!(inner.transport.writes().len(), count);
    let mut readonly = Inner::new(Mock::new(&ATTRIBUTE_PROFILE)).unwrap();
    let caps = readonly.caps(FanId(0)).unwrap();
    assert_eq!(caps.encoding, TargetEncoding::Float32);
    assert!(!caps.validated);
    assert_eq!(caps.min_rpm, 1000.0);
    assert_eq!(caps.max_rpm, 6000.0);
}

// The real transaction engine drives the real adapter logic below. Only the
// private lowest-level SMC transport is an in-memory mock; no real device opens.
struct CoreMockActuator(Rc<RefCell<Inner<Mock>>>);
impl Actuator for CoreMockActuator {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        self.0
            .borrow_mut()
            .inventory()
            .map_err(|error| error.core())
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        self.0.borrow_mut().caps(fan).map_err(|error| error.core())
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        self.0.borrow_mut().state(fan).map_err(|error| error.core())
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        self.0
            .borrow_mut()
            .target(fan, rpm)
            .map_err(|error| error.core())
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        self.0
            .borrow_mut()
            .set_mode(fan, mode)
            .map_err(|error| error.core())
    }
}
struct TestTokens;
impl TokenSource for TestTokens {
    fn next_token(&mut self) -> Result<[u8; 32], TokenError> {
        Ok([0xa5; 32])
    }
}
type TestEngine = Engine<CoreMockActuator, TestTokens>;
fn acquire_core_lease(
    profile: &'static Profile,
) -> (TestEngine, Rc<RefCell<Inner<Mock>>>, Owner, LeaseToken) {
    let inner = Rc::new(RefCell::new(admitted(profile)));
    let mut engine = Engine::new(CoreMockActuator(inner.clone()), TestTokens);
    let owner = Owner::from_authenticated_transport([1; 32]);
    let response = engine
        .execute(
            owner,
            Command::Acquire {
                fan: FanId(0),
                rpm: 3000.0,
                ttl_ms: 5000,
                seq: 1,
            },
            0,
        )
        .unwrap();
    let Response::Lease(grant) = response else {
        panic!("expected real core lease grant")
    };
    assert_eq!(inner.borrow().transport.byte(Key::Gate), 1);
    (engine, inner, owner, grant.token)
}

#[test]
fn core_does_not_confirm_automatic_on_stuck_ftst_and_a_retry_repairs_it() {
    for profile in [&UPPER, &LOWER] {
        let (mut engine, inner, owner, token) = acquire_core_lease(profile);
        inner.borrow_mut().transport.ignore = Some(Key::Gate);
        assert!(engine
            .execute(
                owner,
                Command::Release {
                    fan: FanId(0),
                    token,
                    seq: 2
                },
                1
            )
            .is_err());
        assert!(matches!(
            engine.status(FanId(0)).unwrap().phase,
            Phase::Recovering {
                attempts: 1,
                next_retry_at_ms: Some(251),
                exhausted: false,
                ..
            }
        ));
        assert_eq!(
            inner
                .borrow()
                .transport
                .byte(direct_key(profile.route, FanId(0))),
            0
        );
        assert_eq!(inner.borrow().transport.byte(Key::Gate), 1);
        assert!(
            matches!(inner.borrow_mut().state(FanId(0)), Err(AdapterError::ReadbackMismatch {key}) if key=="Ftst")
        );
        inner.borrow_mut().transport.ignore = None;
        engine
            .execute(owner, Command::Tick(TickReason::Periodic), 251)
            .unwrap();
        assert!(matches!(
            engine.status(FanId(0)).unwrap().phase,
            Phase::AutomaticConfirmed { .. }
        ));
        assert_eq!(inner.borrow().transport.byte(Key::Gate), 0);
        assert_eq!(
            inner.borrow_mut().state(FanId(0)).unwrap().mode,
            Mode::Automatic
        );
        let gate_writes: Vec<_> = inner
            .borrow()
            .transport
            .writes()
            .into_iter()
            .filter(|event| event.starts_with("W:Ftst:"))
            .collect();
        assert_eq!(gate_writes, ["W:Ftst:01", "W:Ftst:00", "W:Ftst:00"]);
    }
}

#[test]
fn core_exhausts_recovery_without_false_automatic_when_ftst_keeps_sticking() {
    for profile in [&UPPER, &LOWER] {
        let (mut engine, inner, owner, token) = acquire_core_lease(profile);
        inner.borrow_mut().transport.ignore = Some(Key::Gate);
        assert!(engine
            .execute(
                owner,
                Command::Release {
                    fan: FanId(0),
                    token,
                    seq: 2
                },
                1
            )
            .is_err());
        for (now, attempts, next) in [(251, 2, Some(501)), (501, 3, None)] {
            engine
                .execute(owner, Command::Tick(TickReason::Periodic), now)
                .unwrap();
            assert!(
                matches!(engine.status(FanId(0)).unwrap().phase, Phase::Recovering { attempts:actual, next_retry_at_ms:actual_next, exhausted, .. } if actual==attempts && actual_next==next && exhausted==(attempts==3))
            );
            assert_eq!(inner.borrow().transport.byte(Key::Gate), 1);
            assert!(inner.borrow_mut().state(FanId(0)).is_err());
        }
        let writes_before = inner.borrow().transport.writes();
        engine
            .execute(owner, Command::Tick(TickReason::Periodic), 751)
            .unwrap();
        assert!(matches!(
            engine.status(FanId(0)).unwrap().phase,
            Phase::Recovering {
                attempts: 3,
                exhausted: true,
                ..
            }
        ));
        assert_eq!(inner.borrow().transport.writes(), writes_before);
        let gate_writes: Vec<_> = writes_before
            .into_iter()
            .filter(|event| event.starts_with("W:Ftst:"))
            .collect();
        assert_eq!(
            gate_writes,
            ["W:Ftst:01", "W:Ftst:00", "W:Ftst:00", "W:Ftst:00"]
        );
    }
}

#[test]
fn ftst_state_checks_all_fans_and_automatic_repair_preserves_other_manual_fan() {
    for profile in [&UPPER, &LOWER] {
        let mut inner = admitted(profile);
        let other_key = direct_key(profile.route, FanId(1));
        let metadata = profile.fan(FanId(1)).unwrap().mode_metadata;
        inner.transport.keys.insert(other_key, (metadata, vec![1]));
        // Direct fan0=auto and Ftst=0 are individually plausible, but together
        // contradict fan1=manual; no fan can be reported automatic as confirmed.
        assert!(
            matches!(inner.state(FanId(0)),Err(AdapterError::ReadbackMismatch {key}) if key=="Ftst")
        );
        inner.set_mode(FanId(0), WriteMode::Automatic).unwrap();
        assert_eq!(inner.transport.byte(other_key), 1);
        assert_eq!(inner.transport.byte(Key::Gate), 1);
        assert_eq!(inner.state(FanId(0)).unwrap().mode, Mode::Automatic);
        assert_eq!(inner.state(FanId(1)).unwrap().mode, Mode::Manual);
    }
}

#[test]
fn control_admission_is_readonly_and_checks_the_complete_inventory() {
    for profile in [&UPPER, &LOWER, &MASK, &ATTRIBUTE_PROFILE] {
        let mut inner = admitted(profile);
        assert_eq!(inner.control_admission(), ControlAdmission::Admitted);
        assert!(inner.transport.writes().is_empty());
        // The second fan must be validated, including exact metadata attributes.
        inner
            .transport
            .keys
            .get_mut(&Key::Target(FanId(1)))
            .unwrap()
            .0
            .attributes ^= 1;
        assert_eq!(
            inner.control_admission(),
            ControlAdmission::ProfileValidationFailed
        );
        assert!(inner.transport.writes().is_empty());
    }
    let mut inner = Inner::new(Mock::new(&UPPER)).unwrap();
    assert_eq!(
        inner.control_admission(),
        ControlAdmission::NoReviewedProfile
    );
    assert!(inner.transport.writes().is_empty());
}

#[test]
fn control_admission_rejects_inventory_identity_and_mode_drift() {
    for fault in 0..5 {
        let mut inner = admitted(&UPPER);
        match fault {
            0 => {
                inner.transport.keys.get_mut(&Key::Count).unwrap().1 = vec![1];
            }
            1 => {
                inner.transport.keys.get_mut(&Key::Count).unwrap().1 = vec![0];
            }
            2 => {
                inner.transport.identity.model = "ChangedMac".into();
            }
            3 => {
                inner
                    .transport
                    .keys
                    .get_mut(&Key::ModeUpper(FanId(1)))
                    .unwrap()
                    .1 = vec![2];
            }
            _ => {
                inner.transport.keys.get_mut(&Key::Gate).unwrap().1 = vec![1];
            }
        }
        assert_eq!(
            inner.control_admission(),
            ControlAdmission::ProfileValidationFailed
        );
        assert!(inner.transport.writes().is_empty());
    }
    let mut inner = admitted(&UPPER);
    inner.profile = None; // Same irreversible clear performed by readonly entry points.
    assert_eq!(
        inner.control_admission(),
        ControlAdmission::NoReviewedProfile
    );
    assert!(inner.target(FanId(1), 3000.0).is_err());
    assert!(inner.set_mode(FanId(1), WriteMode::Manual).is_err());
    assert!(inner.transport.writes().is_empty());
}

#[test]
fn admitted_mock_demotes_irreversibly_including_inspection_and_capabilities() {
    let mut inner = admitted(&UPPER);
    assert_eq!(inner.control_admission(), ControlAdmission::Admitted);
    assert!(inner.probe(0).hardware_profile_verified);
    assert!(inner.caps(FanId(1)).unwrap().validated);
    let mut readonly = inner.into_readonly();
    for _ in 0..2 {
        assert_eq!(
            readonly.control_admission(),
            ControlAdmission::NoReviewedProfile
        );
        assert!(!readonly.probe(0).hardware_profile_verified);
        for fan in [FanId(0), FanId(1)] {
            assert!(!readonly.caps(fan).unwrap().validated);
            assert!(readonly.target(fan, 3000.0).is_err());
            assert!(readonly.set_mode(fan, WriteMode::Manual).is_err());
            assert!(readonly.set_mode(fan, WriteMode::Automatic).is_err());
        }
        assert!(readonly.profile.is_none());
        assert!(readonly.transport.writes().is_empty());
    }
}
