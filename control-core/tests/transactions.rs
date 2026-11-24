use macs_fan_control_core::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

const F: FanId = FanId(0);
const G: FanId = FanId(1);
fn owner(id: u8) -> Owner {
    Owner::from_authenticated_transport([id; 32])
}
fn caps(fan: FanId) -> Capabilities {
    Capabilities {
        fan,
        validated: true,
        model: ModelSupport::Verified,
        encoding: TargetEncoding::Fpe2,
        mode_control: ModeControl::PerFan,
        min_rpm: 1000.0,
        max_rpm: 5000.0,
        step_rpm: 0.25,
        stages_target_while_automatic: true,
    }
}
fn automatic() -> HardwareState {
    HardwareState {
        mode: Mode::Automatic,
        target_rpm: Some(1500.0),
    }
}
fn manual(rpm: f64) -> HardwareState {
    HardwareState {
        mode: Mode::Manual,
        target_rpm: Some(rpm),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Enumerate,
    Capabilities,
    Read,
    Target,
    Mode(WriteMode),
}
#[derive(Clone, Copy)]
enum Effect {
    Pass,
    Fail,
    PartialFail,
    Ignore,
    Read(HardwareState),
    ReadAndSet(HardwareState),
}
struct Device {
    inventory: Vec<FanId>,
    caps: BTreeMap<FanId, Capabilities>,
    states: BTreeMap<FanId, HardwareState>,
    script: VecDeque<(Op, Effect)>,
    events: Vec<(FanId, Op)>,
}
impl Device {
    fn effect(&mut self, fan: FanId, op: Op) -> Effect {
        self.events.push((fan, op));
        if self
            .script
            .front()
            .is_some_and(|(expected, _)| *expected == op)
        {
            self.script.pop_front().unwrap().1
        } else {
            Effect::Pass
        }
    }
    fn writes(&self) -> Vec<(FanId, Op)> {
        self.events
            .iter()
            .copied()
            .filter(|(_, op)| matches!(op, Op::Target | Op::Mode(_)))
            .collect()
    }
}
#[derive(Clone)]
struct Fake(Arc<Mutex<Device>>);
impl Actuator for Fake {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        let mut d = self.0.lock().unwrap();
        if matches!(d.effect(F, Op::Enumerate), Effect::Fail) {
            Err(ActuatorError::Io)
        } else {
            Ok(d.inventory.clone())
        }
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        let mut d = self.0.lock().unwrap();
        if matches!(d.effect(fan, Op::Capabilities), Effect::Fail) {
            Err(ActuatorError::Io)
        } else {
            d.caps.get(&fan).cloned().ok_or(ActuatorError::Unsupported)
        }
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        let mut d = self.0.lock().unwrap();
        match d.effect(fan, Op::Read) {
            Effect::Read(state) => Ok(state),
            Effect::ReadAndSet(state) => {
                d.states.insert(fan, state);
                Ok(state)
            }
            Effect::Fail => Err(ActuatorError::Timeout),
            _ => d
                .states
                .get(&fan)
                .copied()
                .ok_or(ActuatorError::Unavailable),
        }
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        let mut d = self.0.lock().unwrap();
        let effect = d.effect(fan, Op::Target);
        if !matches!(effect, Effect::Fail | Effect::Ignore) {
            d.states.get_mut(&fan).unwrap().target_rpm = Some(rpm);
        }
        if matches!(effect, Effect::Fail | Effect::PartialFail) {
            Err(ActuatorError::Io)
        } else {
            Ok(())
        }
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        let mut d = self.0.lock().unwrap();
        let effect = d.effect(fan, Op::Mode(mode));
        if !matches!(effect, Effect::Fail | Effect::Ignore) {
            d.states.get_mut(&fan).unwrap().mode = match mode {
                WriteMode::Automatic => Mode::Automatic,
                WriteMode::Manual => Mode::Manual,
            };
        }
        if matches!(effect, Effect::Fail | Effect::PartialFail) {
            Err(ActuatorError::Io)
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct Tokens {
    next: u64,
    fail: bool,
    fixed: Option<[u8; 32]>,
}
impl TokenSource for Tokens {
    fn next_token(&mut self) -> Result<[u8; 32], TokenError> {
        if self.fail {
            return Err(TokenError);
        }
        if let Some(bytes) = self.fixed {
            return Ok(bytes);
        }
        self.next += 1;
        let mut bytes = [0xA5; 32];
        bytes[..8].copy_from_slice(&self.next.to_le_bytes());
        Ok(bytes)
    }
}
type TestEngine = Engine<Fake, Tokens>;
fn fixture_with(tokens: Tokens) -> (TestEngine, Arc<Mutex<Device>>) {
    let device = Arc::new(Mutex::new(Device {
        inventory: vec![F, G],
        caps: [(F, caps(F)), (G, caps(G))].into(),
        states: [(F, automatic()), (G, automatic())].into(),
        script: VecDeque::new(),
        events: vec![],
    }));
    (Engine::new(Fake(device.clone()), tokens), device)
}
fn fixture() -> (TestEngine, Arc<Mutex<Device>>) {
    fixture_with(Tokens::default())
}
fn acquire(engine: &mut TestEngine, who: u8, fan: FanId, now: u64, ttl: u64) -> LeaseGrant {
    match engine
        .execute(
            owner(who),
            Command::Acquire {
                fan,
                rpm: 3000.0,
                ttl_ms: ttl,
                seq: 1,
            },
            now,
        )
        .unwrap()
    {
        Response::Lease(grant) => grant,
        other => panic!("unexpected: {other:?}"),
    }
}
fn renew(
    engine: &mut TestEngine,
    who: u8,
    grant: &LeaseGrant,
    now: u64,
    ttl: u64,
    seq: u64,
) -> Result<Response, Error> {
    engine.execute(
        owner(who),
        Command::Renew {
            fan: grant.fan,
            token: grant.token.clone(),
            ttl_ms: ttl,
            seq,
        },
        now,
    )
}
fn release(engine: &mut TestEngine, grant: &LeaseGrant, now: u64) -> Result<Response, Error> {
    engine.execute(
        owner(1),
        Command::Release {
            fan: grant.fan,
            token: grant.token.clone(),
            seq: grant.seq + 1,
        },
        now,
    )
}
fn tick(engine: &mut TestEngine, now: u64) -> Result<Response, Error> {
    engine.execute(owner(99), Command::Tick(TickReason::Periodic), now)
}
fn is_automatic(engine: &TestEngine, fan: FanId) -> bool {
    matches!(
        engine.status(fan).unwrap().phase,
        Phase::AutomaticConfirmed { .. }
    )
}

#[test]
fn transaction_stages_target_before_manual_and_confirms_both() {
    let (mut e, d) = fixture();
    assert_eq!(
        e.execute(owner(1), Command::Enumerate, 0),
        Ok(Response::Fans(vec![F, G]))
    );
    let grant = acquire(&mut e, 1, F, 0, 5000);
    assert_eq!(grant.expires_at_ms, 5000);
    let d = d.lock().unwrap();
    assert_eq!(
        d.events.iter().map(|(_, op)| *op).collect::<Vec<_>>(),
        vec![
            Op::Enumerate,
            Op::Enumerate,
            Op::Capabilities,
            Op::Read,
            Op::Target,
            Op::Read,
            Op::Mode(WriteMode::Manual),
            Op::Read
        ]
    );
    assert_eq!(d.states[&F], manual(3000.0));
}

#[test]
fn hostile_ranges_ttl_sequence_and_fan_cannot_write() {
    for rpm in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        0.0,
        999.75,
        5000.25,
        3000.1,
    ] {
        let (mut e, d) = fixture();
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(Error::InvalidRpm)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
    for (fan, ttl, seq, expected) in [
        (F, 0, 1, Error::InvalidTtl),
        (F, 5001, 1, Error::InvalidTtl),
        (F, 100, 0, Error::InvalidSequence),
        (FanId(65535), 100, 1, Error::UnknownFan),
    ] {
        let (mut e, d) = fixture();
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan,
                    rpm: 3000.0,
                    ttl_ms: ttl,
                    seq
                },
                0
            ),
            Err(expected)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
}

#[test]
fn capabilities_are_self_read_and_unknown_model_mode_encoding_fail_closed() {
    let mut invalid = vec![];
    let mut c = caps(F);
    c.validated = false;
    invalid.push(c);
    let mut c = caps(F);
    c.model = ModelSupport::Unknown;
    invalid.push(c);
    let mut c = caps(F);
    c.mode_control = ModeControl::Unknown;
    invalid.push(c);
    let mut c = caps(F);
    c.encoding = TargetEncoding::Unknown;
    invalid.push(c);
    let mut c = caps(F);
    c.stages_target_while_automatic = false;
    invalid.push(c);
    let mut c = caps(F);
    c.fan = G;
    invalid.push(c);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 5001.0] {
        let mut c = caps(F);
        c.min_rpm = value;
        invalid.push(c);
    }
    for value in [f64::NAN, f64::INFINITY, 999.0] {
        let mut c = caps(F);
        c.max_rpm = value;
        invalid.push(c);
    }
    for value in [
        f64::NAN,
        f64::INFINITY,
        0.0,
        -1.0,
        5001.0,
        f64::MIN_POSITIVE / 2.0,
    ] {
        let mut c = caps(F);
        c.step_rpm = value;
        invalid.push(c);
    }
    for c in invalid {
        let (mut e, d) = fixture();
        d.lock().unwrap().caps.insert(F, c);
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(Error::UnverifiedCapabilities)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
}

#[test]
fn inventory_duplicates_and_read_failures_do_not_create_ownership() {
    let (mut e, d) = fixture();
    d.lock().unwrap().inventory = vec![F, F];
    assert_eq!(
        e.execute(owner(1), Command::Capabilities { fan: F }, 0),
        Err(Error::InvalidInventory)
    );
    assert!(e.status(F).is_none());
    for op in [Op::Enumerate, Op::Capabilities, Op::Read] {
        let (mut e, d) = fixture();
        d.lock().unwrap().script.push_back((op, Effect::Fail));
        assert!(matches!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(Error::Actuator(_))
        ));
        assert!(e.status(F).is_none());
        assert!(d.lock().unwrap().writes().is_empty());
    }
}

#[test]
fn read_command_faults_revoke_existing_leases_and_preserve_failed_recovery() {
    for command in [Command::Enumerate, Command::Capabilities { fan: F }] {
        let (mut e, d) = fixture();
        let grant = acquire(&mut e, 1, F, 0, 1000);
        let op = if matches!(command, Command::Enumerate) {
            Op::Enumerate
        } else {
            Op::Capabilities
        };
        d.lock().unwrap().script = [
            (op, Effect::Fail),
            (Op::Mode(WriteMode::Automatic), Effect::Fail),
        ]
        .into();
        assert!(e.execute(owner(1), command, 1).is_err());
        assert_eq!(
            renew(&mut e, 1, &grant, 2, 1000, 2),
            Err(Error::RecoveryPending)
        );
        d.lock().unwrap().inventory.clear();
        e.execute(owner(1), Command::Enumerate, 3).unwrap();
        assert!(matches!(
            e.status(F).unwrap().phase,
            Phase::Recovering { attempts: 1, .. }
        ));
        tick(&mut e, 251).unwrap();
        assert!(is_automatic(&e, F));
    }
}

#[test]
fn encodings_reject_nonrepresentable_ranges_and_targets_without_rounding() {
    for (encoding, min, max, step, rpm, expected) in [
        (
            TargetEncoding::Fpe2,
            1000.0,
            20000.0,
            0.25,
            3000.0,
            Error::UnverifiedCapabilities,
        ),
        (
            TargetEncoding::Fpe2,
            1000.0,
            5000.0,
            0.1,
            3000.0,
            Error::UnverifiedCapabilities,
        ),
        (
            TargetEncoding::Float32,
            1000.0,
            5000.0,
            0.1,
            3000.0,
            Error::UnverifiedCapabilities,
        ),
        (
            TargetEncoding::Float32,
            1000.0,
            16_777_220.0,
            0.25,
            16_777_217.0,
            Error::InvalidRpm,
        ),
    ] {
        let (mut e, d) = fixture();
        {
            let mut device = d.lock().unwrap();
            let c = device.caps.get_mut(&F).unwrap();
            c.encoding = encoding;
            c.min_rpm = min;
            c.max_rpm = max;
            c.step_rpm = step;
        }
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(expected)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
    let (mut e, d) = fixture();
    d.lock().unwrap().caps.get_mut(&F).unwrap().encoding = TargetEncoding::Float32;
    acquire(&mut e, 1, F, 0, 100);
}

#[test]
fn no_takeover_of_preexisting_manual_or_unknown_mode() {
    for (state, expected) in [
        (manual(2500.0), Error::ExternalController),
        (
            HardwareState {
                mode: Mode::Unknown,
                target_rpm: None,
            },
            Error::UnverifiedCapabilities,
        ),
    ] {
        let (mut e, d) = fixture();
        d.lock().unwrap().states.insert(F, state);
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(expected)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
}

#[test]
fn single_owner_token_and_strictly_increasing_sequence() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 5000);
    assert_eq!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3500.0,
                ttl_ms: 100,
                seq: 2
            },
            1
        ),
        Err(Error::OwnerConflict)
    );
    assert_eq!(
        renew(&mut e, 2, &grant, 2, 100, 2),
        Err(Error::OwnerConflict)
    );
    let forged = LeaseGrant {
        token: LeaseToken::from_transport_bytes([4; 32]),
        ..grant.clone()
    };
    assert_eq!(
        renew(&mut e, 1, &forged, 3, 100, 2),
        Err(Error::InvalidToken)
    );
    for seq in [0, 1] {
        assert_eq!(
            renew(&mut e, 1, &grant, 4, 100, seq),
            Err(Error::InvalidSequence)
        );
    }
    assert!(matches!(
        renew(&mut e, 1, &grant, 5, 100, 3),
        Ok(Response::Lease(_))
    ));
    assert_eq!(
        renew(&mut e, 1, &grant, 6, 100, 2),
        Err(Error::InvalidSequence)
    );
    assert_eq!(
        renew(&mut e, 1, &grant, 7, 100, 3),
        Err(Error::InvalidSequence)
    );
    assert_eq!(
        d.lock().unwrap().writes().len(),
        2,
        "renew never writes target or mode"
    );
}

#[test]
fn renewal_rejects_invalid_ttl_and_checks_device_without_changing_good_lease() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 1000);
    for ttl in [0, 5001] {
        assert_eq!(renew(&mut e, 1, &grant, 1, ttl, 2), Err(Error::InvalidTtl));
    }
    assert_eq!(
        e.status(F).unwrap().phase,
        Phase::Active {
            target_rpm: 3000.0,
            expires_at_ms: 1000,
            seq: 1
        }
    );
    d.lock().unwrap().caps.get_mut(&F).unwrap().max_rpm = 4900.0;
    assert!(matches!(
        renew(&mut e, 1, &grant, 2, 1000, 2),
        Err(Error::Fault {
            cause: Fault::CapabilityChanged,
            ..
        })
    ));
    assert!(is_automatic(&e, F));
    assert_eq!(
        renew(&mut e, 1, &grant, 3, 1000, 3),
        Err(Error::NoActiveLease)
    );
}

#[test]
fn expiry_is_inclusive_and_renew_cannot_revive_expired_token() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 10, 100);
    tick(&mut e, 109).unwrap();
    assert!(matches!(e.status(F).unwrap().phase, Phase::Active { .. }));
    assert_eq!(
        renew(&mut e, 1, &grant, 110, 100, 2),
        Err(Error::NoActiveLease)
    );
    assert!(is_automatic(&e, F));
    assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
    let next = acquire(&mut e, 1, F, 111, 100);
    assert_ne!(grant.token, next.token);
    assert_eq!(
        renew(&mut e, 1, &grant, 112, 100, 2),
        Err(Error::InvalidToken)
    );
}

#[test]
fn expiry_is_serviced_even_on_unrelated_read_command() {
    let (mut e, d) = fixture();
    acquire(&mut e, 1, F, 0, 100);
    e.execute(owner(2), Command::Enumerate, 100).unwrap();
    assert!(is_automatic(&e, F));
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn release_authenticates_and_confirms_automatic() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 5000);
    assert_eq!(
        e.execute(
            owner(2),
            Command::Release {
                fan: F,
                token: grant.token.clone(),
                seq: 2
            },
            1
        ),
        Err(Error::OwnerConflict)
    );
    assert_eq!(
        e.execute(
            owner(1),
            Command::Release {
                fan: F,
                token: grant.token.clone(),
                seq: 1
            },
            2
        ),
        Err(Error::InvalidSequence)
    );
    assert_eq!(release(&mut e, &grant, 3), Ok(Response::AutomaticConfirmed));
    assert!(is_automatic(&e, F));
    assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
    assert_eq!(release(&mut e, &grant, 4), Err(Error::NoActiveLease));
}

#[test]
fn acquisition_partial_write_errors_and_lying_successes_restore_automatic() {
    for (op, effect, cause) in [
        (Op::Target, Effect::Fail, Fault::TargetWrite),
        (Op::Target, Effect::PartialFail, Fault::TargetWrite),
        (Op::Target, Effect::Ignore, Fault::TargetReadback),
        (Op::Mode(WriteMode::Manual), Effect::Fail, Fault::ModeWrite),
        (
            Op::Mode(WriteMode::Manual),
            Effect::PartialFail,
            Fault::ModeWrite,
        ),
        (
            Op::Mode(WriteMode::Manual),
            Effect::Ignore,
            Fault::ModeReadback,
        ),
    ] {
        let (mut e, d) = fixture();
        d.lock().unwrap().script.push_back((op, effect));
        assert!(
            matches!(e.execute(owner(1), Command::Acquire { fan: F, rpm: 3000.0, ttl_ms: 1000, seq: 1 }, 0),
            Err(Error::Fault { cause: got, phase: Phase::AutomaticConfirmed { .. }, .. }) if cause == got)
        );
        assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
        assert!(
            matches!(e.status(F).unwrap().phase, Phase::AutomaticConfirmed { last_fault: Some(got) } if got == cause)
        );
    }
}

#[test]
fn staging_and_final_read_errors_or_wrong_targets_are_not_success() {
    for stage in [1, 2] {
        for effect in [
            Effect::Fail,
            Effect::Read(HardwareState {
                mode: Mode::Unknown,
                target_rpm: None,
            }),
            Effect::Read(HardwareState {
                mode: if stage == 1 {
                    Mode::Automatic
                } else {
                    Mode::Manual
                },
                target_rpm: Some(2999.75),
            }),
        ] {
            let (mut e, d) = fixture();
            let mut script = vec![(Op::Read, Effect::Pass)];
            if stage == 2 {
                script.push((Op::Read, Effect::Pass));
            }
            script.push((Op::Read, effect));
            d.lock().unwrap().script = script.into();
            assert!(matches!(
                e.execute(
                    owner(1),
                    Command::Acquire {
                        fan: F,
                        rpm: 3000.0,
                        ttl_ms: 100,
                        seq: 1
                    },
                    0
                ),
                Err(Error::Fault { .. })
            ));
            assert!(is_automatic(&e, F));
        }
    }
}

#[test]
fn external_takeover_during_target_staging_yields_without_mode_write() {
    let (mut e, d) = fixture();
    d.lock().unwrap().script = [
        (Op::Read, Effect::Pass),
        (Op::Read, Effect::Read(manual(2800.0))),
    ]
    .into();
    assert!(matches!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 100,
                seq: 1
            },
            0
        ),
        Err(Error::Fault {
            cause: Fault::ExternalControl,
            ..
        })
    ));
    assert_eq!(d.lock().unwrap().writes(), vec![(F, Op::Target)]);
}

#[test]
fn failed_automatic_write_or_readback_is_never_reported_as_success() {
    for effect in [Effect::Fail, Effect::Ignore, Effect::PartialFail] {
        let (mut e, d) = fixture();
        let grant = acquire(&mut e, 1, F, 0, 1000);
        d.lock()
            .unwrap()
            .script
            .push_back((Op::Mode(WriteMode::Automatic), effect));
        assert!(matches!(
            release(&mut e, &grant, 1),
            Err(Error::Fault {
                phase: Phase::Recovering { attempts: 1, .. },
                ..
            })
        ));
        assert_eq!(
            renew(&mut e, 1, &grant, 2, 100, 3),
            Err(Error::RecoveryPending)
        );
        tick(&mut e, 251).unwrap();
        assert!(is_automatic(&e, F));
    }
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 1000);
    d.lock().unwrap().script = [
        (Op::Mode(WriteMode::Automatic), Effect::Pass),
        (Op::Read, Effect::Fail),
    ]
    .into();
    assert!(matches!(
        release(&mut e, &grant, 1),
        Err(Error::Fault {
            phase: Phase::Recovering {
                last_error: RecoveryError::Read(ActuatorError::Timeout),
                ..
            },
            ..
        })
    ));
}

#[test]
fn recovery_budget_is_bounded_spaced_and_blocks_new_control() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 5000);
    d.lock().unwrap().script = (0..3)
        .map(|_| (Op::Mode(WriteMode::Automatic), Effect::Fail))
        .collect();
    assert!(release(&mut e, &grant, 10).is_err());
    tick(&mut e, 259).unwrap();
    assert_eq!(d.lock().unwrap().writes().len(), 3);
    tick(&mut e, 260).unwrap();
    tick(&mut e, 510).unwrap();
    assert!(matches!(
        e.status(F).unwrap().phase,
        Phase::Recovering {
            attempts: 3,
            exhausted: true,
            next_retry_at_ms: None,
            ..
        }
    ));
    for at in [760, 10_000, 100_000] {
        tick(&mut e, at).unwrap();
    }
    assert_eq!(d.lock().unwrap().writes().len(), 5);
    assert_eq!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 100,
                seq: 3
            },
            100_001
        ),
        Err(Error::RecoveryPending)
    );
}

#[test]
fn faulted_renewal_revokes_secret_and_cannot_extend_failed_control() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 5000);
    d.lock().unwrap().script = [
        (Op::Read, Effect::Fail),
        (Op::Mode(WriteMode::Automatic), Effect::Fail),
    ]
    .into();
    assert!(matches!(
        renew(&mut e, 1, &grant, 1, 5000, 2),
        Err(Error::Fault {
            cause: Fault::StateRead,
            ..
        })
    ));
    assert_eq!(
        renew(&mut e, 1, &grant, 2, 5000, 3),
        Err(Error::RecoveryPending)
    );
    tick(&mut e, 251).unwrap();
    assert!(is_automatic(&e, F));
    assert_eq!(
        renew(&mut e, 1, &grant, 252, 5000, 4),
        Err(Error::NoActiveLease)
    );
}

#[test]
fn external_mode_or_target_change_never_triggers_a_write_fight() {
    for observed in [automatic(), manual(2500.0), manual(99999.0)] {
        for event in [0, 1, 2, 3] {
            let (mut e, d) = fixture();
            let grant = acquire(&mut e, 1, F, 0, 100);
            d.lock().unwrap().states.insert(F, observed);
            match event {
                0 => {
                    let _ = renew(&mut e, 1, &grant, 1, 100, 2);
                }
                1 => {
                    let _ = release(&mut e, &grant, 1);
                }
                2 => {
                    tick(&mut e, 1).unwrap();
                }
                _ => {
                    tick(&mut e, 100).unwrap();
                }
            }
            for at in [101, 400, 1000] {
                tick(&mut e, at).unwrap();
            }
            assert_eq!(d.lock().unwrap().writes().len(), 2);
            if observed.mode == Mode::Manual {
                assert!(matches!(e.status(F).unwrap().phase, Phase::Yielded { .. }));
            }
        }
    }
}

#[test]
fn recovery_retry_yields_if_another_controller_changes_target() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 5000);
    d.lock()
        .unwrap()
        .script
        .push_back((Op::Mode(WriteMode::Automatic), Effect::Fail));
    assert!(release(&mut e, &grant, 1).is_err());
    d.lock().unwrap().states.insert(F, manual(2000.0));
    tick(&mut e, 251).unwrap();
    tick(&mut e, 1000).unwrap();
    assert!(matches!(e.status(F).unwrap().phase, Phase::Yielded { .. }));
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn committed_lease_capability_fault_checks_competition_before_any_recovery_write() {
    // All capability-fault entry points must use the committed lease's target
    // as their guard, rather than bypassing it based on the fault enum.
    for operation in 0..6 {
        let (mut e, d) = fixture();
        let grant = acquire(&mut e, 1, F, 0, 1000);
        {
            let mut device = d.lock().unwrap();
            device.states.insert(F, manual(2500.0));
            if operation == 5 {
                device.caps.get_mut(&F).unwrap().max_rpm = 4900.0;
            } else {
                let op = if operation == 3 {
                    Op::Enumerate
                } else {
                    Op::Capabilities
                };
                device.script.push_back((op, Effect::Fail));
            }
        }
        match operation {
            0 | 5 => {
                assert!(renew(&mut e, 1, &grant, 1, 1000, 2).is_err());
            }
            1 => {
                tick(&mut e, 1).unwrap();
            }
            2 => {
                assert!(e
                    .execute(owner(1), Command::Capabilities { fan: F }, 1)
                    .is_err());
            }
            3 => {
                assert!(e.execute(owner(1), Command::Enumerate, 1).is_err());
            }
            _ => {
                assert!(release(&mut e, &grant, 1).is_err());
            }
        }
        assert!(
            matches!(e.status(F).unwrap().phase, Phase::Yielded { observed, .. } if observed == manual(2500.0))
        );
        tick(&mut e, 251).unwrap();
        let device = d.lock().unwrap();
        assert_eq!(device.states[&F], manual(2500.0));
        assert_eq!(
            device.writes().len(),
            2,
            "no automatic write may overwrite an external target"
        );
    }
}

#[test]
fn committed_lease_state_read_fault_also_checks_competition_before_recovery_write() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 1000);
    {
        let mut device = d.lock().unwrap();
        device.states.insert(F, manual(2500.0));
        device.script.push_back((Op::Read, Effect::Fail));
    }
    assert!(matches!(
        renew(&mut e, 1, &grant, 1, 1000, 2),
        Err(Error::Fault {
            cause: Fault::StateRead,
            phase: Phase::Yielded { .. },
            ..
        })
    ));
    assert_eq!(d.lock().unwrap().writes().len(), 2);
}

fn fail_acquire_at_real_wrong_target(
    engine: &mut TestEngine,
    device: &Arc<Mutex<Device>>,
    failed_auto_attempts: usize,
    lose_first_recovery_read: bool,
) {
    let mut script = vec![
        (Op::Read, Effect::Pass),
        (Op::Read, Effect::Pass),
        // This changes the actual Fake device, not just one returned sample.
        (Op::Read, Effect::ReadAndSet(manual(1500.0))),
        (Op::Mode(WriteMode::Automatic), Effect::Fail),
    ];
    if lose_first_recovery_read {
        script.push((Op::Read, Effect::Fail));
    }
    script
        .extend((1..failed_auto_attempts).map(|_| (Op::Mode(WriteMode::Automatic), Effect::Fail)));
    device.lock().unwrap().script = script.into();
    assert!(matches!(
        engine.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 1000,
                seq: 1
            },
            0
        ),
        Err(Error::Fault {
            cause: Fault::ModeReadback,
            phase: Phase::Recovering { attempts: 1, .. },
            ..
        })
    ));
    assert_eq!(device.lock().unwrap().states[&F], manual(1500.0));
}

#[test]
fn unconfirmed_wrong_target_is_a_failure_baseline_and_retries_to_automatic() {
    for lose_read in [false, true] {
        let (mut e, d) = fixture();
        fail_acquire_at_real_wrong_target(&mut e, &d, 1, lose_read);
        tick(&mut e, 250).unwrap();
        assert!(is_automatic(&e, F));
        let device = d.lock().unwrap();
        assert_eq!(device.states[&F].mode, Mode::Automatic);
        assert_eq!(
            device.writes().len(),
            4,
            "initial auto failure plus one successful retry"
        );
    }
}

#[test]
fn unchanged_unconfirmed_bad_target_exhausts_full_recovery_budget_without_yielding() {
    let (mut e, d) = fixture();
    fail_acquire_at_real_wrong_target(&mut e, &d, 3, false);
    tick(&mut e, 250).unwrap();
    assert!(matches!(
        e.status(F).unwrap().phase,
        Phase::Recovering {
            attempts: 2,
            exhausted: false,
            ..
        }
    ));
    tick(&mut e, 500).unwrap();
    assert!(matches!(
        e.status(F).unwrap().phase,
        Phase::Recovering {
            attempts: 3,
            exhausted: true,
            ..
        }
    ));
    tick(&mut e, 750).unwrap();
    assert_eq!(d.lock().unwrap().writes().len(), 5);
}

#[test]
fn unconfirmed_recovery_yields_only_after_target_changes_from_failure_site_baseline() {
    let (mut e, d) = fixture();
    fail_acquire_at_real_wrong_target(&mut e, &d, 1, false);
    d.lock().unwrap().states.insert(F, manual(2500.0));
    tick(&mut e, 250).unwrap();
    assert!(
        matches!(e.status(F).unwrap().phase, Phase::Yielded { observed, .. } if observed == manual(2500.0))
    );
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn unconfirmed_failure_site_baseline_is_not_overwritten_by_later_failed_recovery_read() {
    let (mut e, d) = fixture();
    d.lock().unwrap().script = [
        (Op::Read, Effect::Pass),
        (Op::Read, Effect::Pass),
        (Op::Read, Effect::ReadAndSet(manual(1500.0))),
        (Op::Mode(WriteMode::Automatic), Effect::Fail),
        (Op::Read, Effect::ReadAndSet(manual(2500.0))),
    ]
    .into();
    assert!(e
        .execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 1000,
                seq: 1
            },
            0
        )
        .is_err());
    tick(&mut e, 250).unwrap();
    assert!(
        matches!(e.status(F).unwrap().phase, Phase::Yielded { observed, .. } if observed == manual(2500.0))
    );
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn unconfirmed_recovery_without_any_initial_readback_establishes_observed_baseline() {
    let (mut e, d) = fixture();
    d.lock().unwrap().script = [
        (Op::Read, Effect::Pass),
        (Op::Read, Effect::Pass),
        (Op::Mode(WriteMode::Manual), Effect::PartialFail),
        (Op::Mode(WriteMode::Automatic), Effect::Fail),
        (Op::Read, Effect::Fail),
    ]
    .into();
    assert!(e
        .execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 1000,
                seq: 1
            },
            0
        )
        .is_err());
    // With both failing-operation and recovery readbacks unavailable, there
    // was no baseline. The first real observation must not compare to 3000.
    d.lock().unwrap().states.insert(F, manual(1500.0));
    tick(&mut e, 250).unwrap();
    assert!(is_automatic(&e, F));
    assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
}

#[test]
fn disconnect_only_revokes_the_authenticated_owner() {
    let (mut e, d) = fixture();
    acquire(&mut e, 1, F, 0, 5000);
    acquire(&mut e, 2, G, 0, 5000);
    e.execute(
        owner(99),
        Command::Tick(TickReason::Disconnected(owner(1))),
        1,
    )
    .unwrap();
    assert!(is_automatic(&e, F));
    assert!(matches!(e.status(G).unwrap().phase, Phase::Active { .. }));
    assert_eq!(d.lock().unwrap().states[&G], manual(3000.0));
}

#[test]
fn token_is_bound_to_its_fan_even_for_the_same_authenticated_owner() {
    let (mut e, d) = fixture();
    let first = acquire(&mut e, 1, F, 0, 1000);
    let second = acquire(&mut e, 1, G, 0, 1000);
    let wrong_fan = LeaseGrant { fan: G, ..first };
    assert_eq!(
        renew(&mut e, 1, &wrong_fan, 1, 1000, 2),
        Err(Error::InvalidToken)
    );
    assert!(renew(&mut e, 1, &second, 2, 1000, 2).is_ok());
    assert_eq!(d.lock().unwrap().writes().len(), 4);
}

#[test]
fn trusted_control_fault_revokes_only_the_affected_fan() {
    let (mut e, d) = fixture();
    let first = acquire(&mut e, 1, F, 0, 1000);
    let second = acquire(&mut e, 1, G, 0, 1000);
    e.execute(owner(99), Command::Tick(TickReason::ControlFault(F)), 1)
        .unwrap();
    assert_eq!(
        e.status(F).unwrap().phase,
        Phase::AutomaticConfirmed {
            last_fault: Some(Fault::ControlInput)
        }
    );
    assert_eq!(
        renew(&mut e, 1, &first, 2, 1000, 2),
        Err(Error::NoActiveLease)
    );
    assert!(renew(&mut e, 1, &second, 3, 1000, 2).is_ok());
    assert_eq!(d.lock().unwrap().writes().len(), 5);
}

#[test]
fn lifecycle_failure_stays_visible_and_retries_even_while_suspended() {
    for reason in [
        TickReason::Disconnected(owner(1)),
        TickReason::Sleep,
        TickReason::Shutdown,
    ] {
        let (mut e, d) = fixture();
        acquire(&mut e, 1, F, 0, 1000);
        d.lock()
            .unwrap()
            .script
            .push_back((Op::Mode(WriteMode::Automatic), Effect::Fail));
        let result = e.execute(owner(99), Command::Tick(reason), 1).unwrap();
        assert!(
            matches!(result, Response::Statuses(statuses) if matches!(statuses[0].phase, Phase::Recovering { attempts: 1, .. }))
        );
        tick(&mut e, 251).unwrap();
        assert!(is_automatic(&e, F));
        assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
    }
}

#[test]
fn sleep_and_shutdown_revoke_all_and_require_fresh_engine() {
    for reason in [TickReason::Sleep, TickReason::Shutdown] {
        let (mut e, d) = fixture();
        let grant = acquire(&mut e, 1, F, 0, 5000);
        acquire(&mut e, 2, G, 0, 5000);
        e.execute(owner(99), Command::Tick(reason), 1).unwrap();
        assert!(is_automatic(&e, F) && is_automatic(&e, G));
        assert_eq!(renew(&mut e, 1, &grant, 2, 100, 2), Err(Error::Suspended));
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1
                },
                3
            ),
            Err(Error::Suspended)
        );
        assert_eq!(d.lock().unwrap().writes().len(), 6);
    }
}

#[test]
fn clock_rollback_revokes_all_and_permanently_locks_new_manual_control() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 100, 1000);
    acquire(&mut e, 2, G, 100, 1000);
    assert_eq!(tick(&mut e, 50), Err(Error::ClockRollback));
    assert!(is_automatic(&e, F) && is_automatic(&e, G));
    assert_eq!(
        renew(&mut e, 1, &grant, 51, 100, 2),
        Err(Error::ClockUntrusted)
    );
    assert_eq!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 100,
                seq: 1
            },
            101
        ),
        Err(Error::ClockUntrusted)
    );
    assert_eq!(d.lock().unwrap().writes().len(), 6);
}

#[test]
fn clock_rollback_rebases_pending_recovery_without_resetting_budget() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, 10_000, 1000);
    d.lock().unwrap().script = (0..3)
        .map(|_| (Op::Mode(WriteMode::Automatic), Effect::Fail))
        .collect();
    assert!(release(&mut e, &grant, 10_001).is_err());
    assert_eq!(tick(&mut e, 1), Err(Error::ClockRollback));
    tick(&mut e, 251).unwrap();
    tick(&mut e, 501).unwrap();
    assert!(matches!(
        e.status(F).unwrap().phase,
        Phase::Recovering {
            attempts: 3,
            exhausted: true,
            ..
        }
    ));
    assert_eq!(d.lock().unwrap().writes().len(), 5);
}

#[test]
fn recovery_deadline_overflow_is_explicitly_exhausted_and_does_not_loop() {
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, u64::MAX - 100, 100);
    d.lock()
        .unwrap()
        .script
        .push_back((Op::Mode(WriteMode::Automatic), Effect::Fail));
    assert!(release(&mut e, &grant, u64::MAX - 99).is_err());
    assert!(matches!(
        e.status(F).unwrap().phase,
        Phase::Recovering {
            attempts: 1,
            exhausted: true,
            ..
        }
    ));
    tick(&mut e, u64::MAX).unwrap();
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn invalid_active_readback_revokes_control_and_reads_automatic_after_recovery() {
    for state in [
        HardwareState {
            mode: Mode::Unknown,
            target_rpm: None,
        },
        HardwareState {
            mode: Mode::Manual,
            target_rpm: None,
        },
        HardwareState {
            mode: Mode::Manual,
            target_rpm: Some(f64::NAN),
        },
        HardwareState {
            mode: Mode::Manual,
            target_rpm: Some(f64::INFINITY),
        },
    ] {
        let (mut e, d) = fixture();
        let grant = acquire(&mut e, 1, F, 0, 1000);
        d.lock().unwrap().states.insert(F, state);
        assert!(matches!(
            renew(&mut e, 1, &grant, 1, 1000, 2),
            Err(Error::Fault {
                cause: Fault::StateRead,
                ..
            })
        ));
        assert!(is_automatic(&e, F));
        assert_eq!(d.lock().unwrap().states[&F].mode, Mode::Automatic);
    }
}

#[test]
fn deadline_overflow_cannot_wrap_or_write() {
    let (mut e, d) = fixture();
    assert_eq!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 10,
                seq: 1
            },
            u64::MAX - 5
        ),
        Err(Error::DeadlineOverflow)
    );
    assert!(d.lock().unwrap().writes().is_empty());
    let (mut e, d) = fixture();
    let grant = acquire(&mut e, 1, F, u64::MAX - 100, 100);
    assert_eq!(
        renew(&mut e, 1, &grant, u64::MAX - 50, 51, 2),
        Err(Error::DeadlineOverflow)
    );
    tick(&mut e, u64::MAX).unwrap();
    assert!(is_automatic(&e, F));
    assert_eq!(d.lock().unwrap().writes().len(), 3);
}

#[test]
fn zero_failed_and_repeated_entropy_never_reuses_a_lease_secret() {
    for tokens in [
        Tokens {
            fail: true,
            ..Tokens::default()
        },
        Tokens {
            fixed: Some([0; 32]),
            ..Tokens::default()
        },
    ] {
        let (mut e, d) = fixture_with(tokens);
        assert_eq!(
            e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1
                },
                0
            ),
            Err(Error::EntropyUnavailable)
        );
        assert!(d.lock().unwrap().writes().is_empty());
    }
    let (mut e, d) = fixture_with(Tokens {
        fixed: Some([7; 32]),
        ..Tokens::default()
    });
    let grant = acquire(&mut e, 1, F, 0, 100);
    release(&mut e, &grant, 1).unwrap();
    assert_eq!(
        e.execute(
            owner(1),
            Command::Acquire {
                fan: F,
                rpm: 3000.0,
                ttl_ms: 100,
                seq: 1
            },
            2
        ),
        Err(Error::EntropyUnavailable)
    );
    assert_eq!(d.lock().unwrap().writes().len(), 3);
    assert!(!format!("{grant:?}").contains("7, 7"));
    assert_eq!(format!("{:?}", grant.token), "LeaseToken([redacted])");
}

#[test]
fn sequence_maximum_requires_release_by_expiry_not_wraparound() {
    let (mut e, _) = fixture();
    let grant = acquire(&mut e, 1, F, 0, 100);
    assert!(renew(&mut e, 1, &grant, 1, 100, u64::MAX).is_ok());
    for seq in [0, 1, u64::MAX] {
        assert_eq!(
            renew(&mut e, 1, &grant, 2, 100, seq),
            Err(Error::InvalidSequence)
        );
    }
    tick(&mut e, 101).unwrap();
    assert!(is_automatic(&e, F));
}

#[test]
fn separate_fans_can_have_separate_owners_but_parallel_same_fan_is_serialized() {
    let (e, d) = fixture();
    let shared = Arc::new(Mutex::new(e));
    let threads: Vec<_> = (1..=8)
        .map(|id| {
            let shared = shared.clone();
            std::thread::spawn(move || {
                shared.lock().unwrap().execute(
                    owner(id),
                    Command::Acquire {
                        fan: F,
                        rpm: 3000.0,
                        ttl_ms: 100,
                        seq: 1,
                    },
                    0,
                )
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(Error::OwnerConflict))
            .count(),
        7
    );
    assert_eq!(d.lock().unwrap().writes().len(), 2);
    acquire(&mut shared.lock().unwrap(), 42, G, 0, 100);
    assert_eq!(d.lock().unwrap().writes().len(), 4);
}

#[test]
fn deterministic_long_sequence_preserves_manual_requires_live_lease_invariant() {
    let (mut e, d) = fixture();
    let mut grant: Option<LeaseGrant> = None;
    let mut seq = 1;
    for now in 0..2000 {
        if now % 41 == 0 {
            if let Ok(Response::Lease(g)) = e.execute(
                owner(1),
                Command::Acquire {
                    fan: F,
                    rpm: 3000.0,
                    ttl_ms: 10,
                    seq: 1,
                },
                now,
            ) {
                grant = Some(g);
                seq = 1;
            }
        } else if now % 7 == 0 {
            if let Some(g) = &grant {
                seq += 1;
                let _ = renew(&mut e, 1, g, now, 10, seq);
            }
        } else if now % 13 == 0 {
            if let Some(g) = &grant {
                seq += 1;
                let _ = e.execute(
                    owner(1),
                    Command::Release {
                        fan: F,
                        token: g.token.clone(),
                        seq,
                    },
                    now,
                );
            }
        }
        tick(&mut e, now).unwrap();
        let state = d.lock().unwrap().states[&F];
        if state.mode == Mode::Manual {
            assert!(
                matches!(e.status(F).unwrap().phase, Phase::Active { expires_at_ms, .. } if expires_at_ms > now)
            );
        } else if e.status(F).is_some() {
            assert!(is_automatic(&e, F));
        }
    }
}
