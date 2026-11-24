use macs_fan_control_core::*;
use macs_fan_control_runtime::{policy, *};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Default)]
struct Device {
    states: BTreeMap<FanId, HardwareState>,
    writes: Vec<(FanId, &'static str, f64)>,
    fail_target: Option<FanId>,
    fail_auto: bool,
    bad_caps: Option<FanId>,
    advance_on_target: u64,
    advance_on_read: u64,
    clock_on_target: Option<u64>,
}
#[derive(Clone)]
struct FakeClock(Rc<RefCell<u64>>);
impl Clock for FakeClock {
    fn now_ms(&mut self) -> u64 {
        *self.0.borrow()
    }
}
struct FakeActuator {
    device: Rc<RefCell<Device>>,
    clock: FakeClock,
}
impl Actuator for FakeActuator {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        Ok(self.device.borrow().states.keys().copied().collect())
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        Ok(Capabilities {
            fan,
            validated: self.device.borrow().bad_caps != Some(fan),
            model: ModelSupport::Verified,
            encoding: TargetEncoding::Fpe2,
            mode_control: ModeControl::PerFan,
            min_rpm: 1000.0,
            max_rpm: 5000.0,
            step_rpm: 1.0,
            stages_target_while_automatic: true,
        })
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        *self.clock.0.borrow_mut() += std::mem::take(&mut self.device.borrow_mut().advance_on_read);
        self.device
            .borrow()
            .states
            .get(&fan)
            .copied()
            .ok_or(ActuatorError::Unavailable)
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        let mut d = self.device.borrow_mut();
        d.writes.push((fan, "target", rpm));
        d.states.get_mut(&fan).unwrap().target_rpm = Some(rpm);
        *self.clock.0.borrow_mut() += d.advance_on_target;
        if let Some(now) = d.clock_on_target.take() {
            *self.clock.0.borrow_mut() = now;
        }
        if d.fail_target == Some(fan) {
            Err(ActuatorError::Io)
        } else {
            Ok(())
        }
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        let mut d = self.device.borrow_mut();
        d.writes.push((
            fan,
            if mode == WriteMode::Automatic {
                "automatic"
            } else {
                "manual"
            },
            0.0,
        ));
        if mode == WriteMode::Automatic && d.fail_auto {
            return Err(ActuatorError::Io);
        }
        d.states.get_mut(&fan).unwrap().mode = if mode == WriteMode::Automatic {
            Mode::Automatic
        } else {
            Mode::Manual
        };
        Ok(())
    }
}
struct Tokens(u8);
impl TokenSource for Tokens {
    fn next_token(&mut self) -> Result<[u8; 32], TokenError> {
        self.0 += 1;
        Ok([self.0; 32])
    }
}
type Runtime = Coordinator<FakeActuator, Tokens, FakeClock>;
fn fixture() -> (Runtime, Rc<RefCell<Device>>, FakeClock) {
    let clock = FakeClock(Rc::new(RefCell::new(0)));
    let device = Rc::new(RefCell::new(Device::default()));
    for fan in 0..3 {
        device.borrow_mut().states.insert(
            FanId(fan),
            HardwareState {
                mode: Mode::Automatic,
                target_rpm: None,
            },
        );
    }
    let runtime = Coordinator::new(
        FakeActuator {
            device: device.clone(),
            clock: clock.clone(),
        },
        Tokens(0),
        clock.clone(),
    );
    (runtime, device, clock)
}
fn owner() -> Owner {
    Owner::from_authenticated_transport([91; 32])
}
fn config() -> policy::Config {
    policy::Config {
        min_rpm: 1000.0,
        max_rpm: 5000.0,
        low_celsius: 40.0,
        high_celsius: 80.0,
        sample_timeout_ms: 1000,
        descent_hold_ms: 2000,
        descent_rpm_per_second: 100.0,
        recovery_samples: 1,
    }
}
fn sample(temp: f64, at: u64) -> Vec<SensorReading> {
    vec![SensorReading {
        sensor: SensorId("cpu.package".into()),
        sample: policy::Sample {
            celsius: temp,
            sampled_at_ms: at,
            quality: policy::Quality::Verified,
        },
    }]
}
fn fixed(fan: u16) -> FanPlan {
    FanPlan {
        fan: FanId(fan),
        strategy: Strategy::Fixed { rpm: 2000.0 },
    }
}
fn sensor(fan: u16) -> FanPlan {
    FanPlan {
        fan: FanId(fan),
        strategy: Strategy::Sensor {
            sensor: SensorId("cpu.package".into()),
            config: config(),
        },
    }
}
fn plan(entries: Vec<FanPlan>) -> Plan {
    Plan {
        entries,
        ttl_ms: 5000,
    }
}
fn target(snapshot: &Snapshot, fan: usize) -> f64 {
    match snapshot.fans[fan].1 {
        FanOutcome::Active { target_rpm, .. } => target_rpm,
        ref phase => panic!("{phase:?}"),
    }
}
fn all_auto(snapshot: &Snapshot) -> bool {
    snapshot
        .fans
        .iter()
        .all(|(_, o)| *o == FanOutcome::AutomaticConfirmed)
}

#[test]
fn full_preset_fixed_sensor_and_observe_only_automatic() {
    let (mut r, d, c) = fixture();
    let p = plan(vec![
        fixed(0),
        sensor(1),
        FanPlan {
            fan: FanId(2),
            strategy: Strategy::Automatic,
        },
    ]);
    assert_eq!(r.snapshot().phase, SessionPhase::Idle);
    assert!(d.borrow().writes.is_empty());
    let s = r.start(owner(), p, &sample(40.0, 0)).unwrap();
    assert_eq!(s.phase, SessionPhase::Running);
    assert_eq!(target(&s, 0), 2000.0);
    assert_eq!(target(&s, 1), 1000.0);
    assert_eq!(s.fans[2].1, FanOutcome::Unobserved);
    let writes = d.borrow().writes.len();
    *c.0.borrow_mut() = 100;
    let s = r.step(&sample(80.0, 100)).unwrap();
    assert_eq!(target(&s, 1), 5000.0);
    // Fixed renewal performs no write, sensor changes target without mode churn.
    assert_eq!(d.borrow().writes.len(), writes + 1);
    *c.0.borrow_mut() = 200;
    r.step(&sample(40.0, 200)).unwrap();
    for at in [1000, 1900] {
        *c.0.borrow_mut() = at;
        r.step(&sample(40.0, at)).unwrap();
    }
    *c.0.borrow_mut() = 2200;
    assert_eq!(target(&r.step(&sample(40.0, 2200)).unwrap(), 1), 5000.0);
    *c.0.borrow_mut() = 2700;
    assert_eq!(target(&r.step(&sample(40.0, 2700)).unwrap(), 1), 4950.0);
    *c.0.borrow_mut() = 2701;
    assert_eq!(target(&r.step(&sample(80.0, 2701)).unwrap(), 1), 5000.0);
    let stopped = r.stop(StopReason::Explicit).unwrap();
    assert_eq!(stopped.fans[0].1, FanOutcome::AutomaticConfirmed);
    assert_eq!(stopped.fans[1].1, FanOutcome::AutomaticConfirmed);
    assert_eq!(stopped.fans[2].1, FanOutcome::Unobserved);
}

#[test]
fn startup_recovery_samples_never_partially_start_a_fixed_fan() {
    let (mut r, d, c) = fixture();
    let mut entry = sensor(1);
    if let Strategy::Sensor { config, .. } = &mut entry.strategy {
        config.recovery_samples = 3;
    }
    assert_eq!(
        r.start(owner(), plan(vec![fixed(0), entry]), &sample(60.0, 0))
            .unwrap()
            .phase,
        SessionPhase::Preparing
    );
    assert!(d.borrow().writes.is_empty());
    *c.0.borrow_mut() = 1;
    assert_eq!(
        r.step(&sample(60.0, 1)).unwrap().phase,
        SessionPhase::Preparing
    );
    assert!(d.borrow().writes.is_empty());
    *c.0.borrow_mut() = 2;
    assert_eq!(
        r.step(&sample(60.0, 2)).unwrap().phase,
        SessionPhase::Running
    );
    assert_eq!(d.borrow().writes.len(), 4);
}

#[test]
fn preflight_rejects_entire_plan_before_any_write() {
    for bad in 0..7 {
        let (mut r, d, _) = fixture();
        let mut p = plan(vec![fixed(0), sensor(1)]);
        let mut samples = sample(60.0, 0);
        match bad {
            0 => p.entries[1].fan = FanId(0),
            1 => p.entries[1].fan = FanId(99),
            2 => d.borrow_mut().bad_caps = Some(FanId(1)),
            3 => samples[0].sample.quality = policy::Quality::Unverified,
            4 => p.ttl_ms = 5001,
            5 => p.entries[1].strategy = Strategy::Fixed { rpm: 2000.25 },
            6 => p.entries = (0..17).map(fixed).collect(),
            _ => unreachable!(),
        }
        assert!(r.start(owner(), p, &samples).is_err(), "case {bad}");
        assert!(d.borrow().writes.is_empty(), "case {bad}");
        assert_eq!(r.snapshot().phase, SessionPhase::Idle);
    }
}

#[test]
fn sensor_faults_stop_all_owned_fans_and_never_auto_resume() {
    for bad in 0..7 {
        let (mut r, d, c) = fixture();
        r.start(owner(), plan(vec![fixed(0), sensor(1)]), &sample(60.0, 0))
            .unwrap();
        *c.0.borrow_mut() = 100;
        let mut samples = sample(70.0, 100);
        match bad {
            0 => samples.clear(),
            1 => samples[0].sample.quality = policy::Quality::Failed,
            2 => samples[0].sample.sampled_at_ms = 101,
            3 => samples[0].sample.sampled_at_ms = 0,
            4 => samples[0].sample.celsius = f64::NAN,
            5 => samples[0].sample.celsius = 128.0,
            6 => {
                *c.0.borrow_mut() = 1200;
                samples[0].sample.sampled_at_ms = 1;
            }
            _ => unreachable!(),
        }
        assert!(r.step(&samples).is_err(), "case {bad}");
        assert!(all_auto(&r.snapshot()), "case {bad}");
        assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
        let writes = d.borrow().writes.len();
        *c.0.borrow_mut() = 1300;
        r.step(&sample(80.0, 1300)).unwrap();
        assert_eq!(writes, d.borrow().writes.len());
    }
}

#[test]
fn multi_fan_acquire_failure_rolls_back_already_acquired_and_partial_write() {
    let (mut r, d, _) = fixture();
    d.borrow_mut().fail_target = Some(FanId(1));
    assert!(r
        .start(owner(), plan(vec![fixed(0), fixed(1)]), &[])
        .is_err());
    assert!(all_auto(&r.snapshot()));
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    assert!(d
        .borrow()
        .states
        .values()
        .all(|s| s.mode == Mode::Automatic));
}

#[test]
fn multi_fan_update_failure_stops_unchanged_fixed_fan_too() {
    let (mut r, d, c) = fixture();
    r.start(owner(), plan(vec![fixed(0), sensor(1)]), &sample(60.0, 0))
        .unwrap();
    d.borrow_mut().fail_target = Some(FanId(1));
    *c.0.borrow_mut() = 1;
    assert!(r.step(&sample(80.0, 1)).is_err());
    assert!(all_auto(&r.snapshot()));
}

#[test]
fn external_controller_yields_without_fighting_it_and_blocks_restart() {
    let (mut r, d, c) = fixture();
    r.start(owner(), plan(vec![fixed(0), fixed(1)]), &[])
        .unwrap();
    d.borrow_mut().states.get_mut(&FanId(0)).unwrap().target_rpm = Some(3300.0);
    *c.0.borrow_mut() = 1;
    assert!(r.watchdog().is_err());
    assert_eq!(r.snapshot().fans[0].1, FanOutcome::Yielded);
    assert_eq!(r.snapshot().fans[1].1, FanOutcome::AutomaticConfirmed);
    assert_eq!(d.borrow().states[&FanId(0)].target_rpm, Some(3300.0));
    assert_eq!(d.borrow().states[&FanId(0)].mode, Mode::Manual);
    assert_eq!(
        r.start(owner(), plan(vec![fixed(1)]), &[]),
        Err(macs_fan_control_runtime::Error::Busy)
    );
}

#[test]
fn never_owned_automatic_entry_does_not_touch_external_manual_mode() {
    let (mut r, d, _) = fixture();
    d.borrow_mut().states.get_mut(&FanId(0)).unwrap().mode = Mode::Manual;
    let p = plan(vec![FanPlan {
        fan: FanId(0),
        strategy: Strategy::Automatic,
    }]);
    assert_eq!(
        r.start(owner(), p, &[]).unwrap().fans[0].1,
        FanOutcome::Unobserved
    );
    r.stop(StopReason::Explicit).unwrap();
    assert!(d.borrow().writes.is_empty());
    assert_eq!(d.borrow().states[&FanId(0)].mode, Mode::Manual);
}

#[test]
fn ttl_expiry_is_independent_of_samples_and_does_not_restart() {
    let (mut r, d, c) = fixture();
    r.start(
        owner(),
        Plan {
            entries: vec![fixed(0)],
            ttl_ms: 100,
        },
        &[],
    )
    .unwrap();
    *c.0.borrow_mut() = 100;
    assert!(r.watchdog().is_err());
    assert!(all_auto(&r.snapshot()));
    let writes = d.borrow().writes.len();
    r.step(&[]).unwrap();
    assert_eq!(writes, d.borrow().writes.len());
}

#[test]
fn overflowing_deadline_and_clock_rollback_fail_closed() {
    let (mut r, d, c) = fixture();
    *c.0.borrow_mut() = u64::MAX - 10;
    assert!(r.start(owner(), plan(vec![fixed(0)]), &[]).is_err());
    assert!(d.borrow().writes.is_empty());
    let (mut r, _, c) = fixture();
    *c.0.borrow_mut() = 100;
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    *c.0.borrow_mut() = 99;
    assert!(r.watchdog().is_err());
    assert!(all_auto(&r.snapshot()));
    *c.0.borrow_mut() = 101;
    assert!(r.start(owner(), plan(vec![fixed(0)]), &[]).is_err());
}

#[test]
fn recovery_pending_retries_then_exhausts_without_false_success() {
    let (mut r, d, c) = fixture();
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    d.borrow_mut().fail_auto = true;
    let s = r.stop(StopReason::Explicit).unwrap();
    assert_eq!(s.fans[0].1, FanOutcome::RecoveryPending);
    assert!(r.start(owner(), plan(vec![fixed(1)]), &[]).is_err());
    for now in [250, 500] {
        *c.0.borrow_mut() = now;
        r.watchdog().unwrap();
    }
    assert_eq!(r.snapshot().fans[0].1, FanOutcome::RecoveryExhausted);
    let writes = d.borrow().writes.len();
    *c.0.borrow_mut() = 1000;
    r.watchdog().unwrap();
    assert_eq!(writes, d.borrow().writes.len());
}

#[test]
fn recovery_success_requires_readback_then_explicit_new_start() {
    let (mut r, d, c) = fixture();
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    d.borrow_mut().fail_auto = true;
    r.stop(StopReason::Explicit).unwrap();
    d.borrow_mut().fail_auto = false;
    *c.0.borrow_mut() = 250;
    assert!(all_auto(&r.watchdog().unwrap()));
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    assert_eq!(r.snapshot().phase, SessionPhase::Running);
}

#[test]
fn sleep_shutdown_disconnect_and_stop_revoke_without_auto_restart() {
    for reason in [
        StopReason::Explicit,
        StopReason::Disconnected,
        StopReason::Sleep,
        StopReason::Shutdown,
    ] {
        let (mut r, d, c) = fixture();
        r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
        assert!(all_auto(&r.stop(reason).unwrap()));
        let writes = d.borrow().writes.len();
        *c.0.borrow_mut() = 1;
        r.step(&[]).unwrap();
        assert_eq!(writes, d.borrow().writes.len());
        if matches!(reason, StopReason::Sleep | StopReason::Shutdown) {
            assert!(r.start(owner(), plan(vec![fixed(0)]), &[]).is_err());
        }
    }
}

#[test]
fn delayed_io_cannot_publish_expired_grants() {
    let (mut r, d, _) = fixture();
    d.borrow_mut().advance_on_target = 100;
    assert!(r
        .start(
            owner(),
            Plan {
                entries: vec![fixed(0), fixed(1)],
                ttl_ms: 100
            },
            &[]
        )
        .is_err());
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    assert_eq!(r.snapshot().fans[0].1, FanOutcome::AutomaticConfirmed);
    assert_eq!(r.snapshot().fans[1].1, FanOutcome::Unobserved);
    assert_eq!(d.borrow().states[&FanId(0)].mode, Mode::Automatic);
}

#[test]
fn slow_io_invalidates_sensor_samples_before_a_second_fan_write() {
    let (mut r, d, _) = fixture();
    d.borrow_mut().advance_on_target = 1001;
    assert!(r
        .start(owner(), plan(vec![fixed(0), sensor(1)]), &sample(60.0, 0))
        .is_err());
    assert_eq!(r.snapshot().fans[0].1, FanOutcome::AutomaticConfirmed);
    assert_eq!(r.snapshot().fans[1].1, FanOutcome::Unobserved);
    assert!(!d.borrow().writes.iter().any(|(fan, _, _)| *fan == FanId(1)));
}

#[test]
fn quantizes_continuous_sensor_curve_and_rejects_replay_across_restart() {
    let (mut r, _, c) = fixture();
    assert_eq!(
        target(
            &r.start(owner(), plan(vec![sensor(0)]), &sample(40.123, 0))
                .unwrap(),
            0
        ),
        1012.0
    );
    r.stop(StopReason::Explicit).unwrap();
    *c.0.borrow_mut() = 1;
    assert!(r
        .start(owner(), plan(vec![sensor(0)]), &sample(40.123, 0))
        .is_err());
    assert!(r
        .start(owner(), plan(vec![sensor(0)]), &sample(40.123, 1))
        .is_ok());
}

#[test]
fn switching_to_fixed_plan_does_not_erase_sensor_replay_history() {
    let (mut r, _, c) = fixture();
    r.start(owner(), plan(vec![sensor(0)]), &sample(60.0, 0))
        .unwrap();
    r.stop(StopReason::Explicit).unwrap();
    *c.0.borrow_mut() = 1;
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    r.stop(StopReason::Explicit).unwrap();
    assert!(r
        .start(owner(), plan(vec![sensor(0)]), &sample(60.0, 0))
        .is_err());
}

#[test]
fn invalid_second_sensor_preflight_never_applies_first_fan() {
    let (mut r, d, _) = fixture();
    let mut second = sensor(1);
    if let Strategy::Sensor { sensor, .. } = &mut second.strategy {
        sensor.0 = "gpu.package".into();
    }
    assert!(r
        .start(owner(), plan(vec![sensor(0), second]), &sample(60.0, 0))
        .is_err());
    assert!(d.borrow().writes.is_empty());
}

#[test]
fn expiration_between_post_io_check_and_final_return_is_not_running() {
    struct AdvancingClock(FakeClock);
    impl Clock for AdvancingClock {
        fn now_ms(&mut self) -> u64 {
            let now = *self.0 .0.borrow();
            *self.0 .0.borrow_mut() += 1;
            now
        }
    }
    let clock = FakeClock(Rc::new(RefCell::new(0)));
    let device = Rc::new(RefCell::new(Device::default()));
    device.borrow_mut().states.insert(
        FanId(0),
        HardwareState {
            mode: Mode::Automatic,
            target_rpm: None,
        },
    );
    let mut runtime = Coordinator::new(
        FakeActuator {
            device: device.clone(),
            clock: clock.clone(),
        },
        Tokens(0),
        AdvancingClock(clock),
    );
    assert!(runtime
        .start(
            owner(),
            Plan {
                entries: vec![fixed(0)],
                ttl_ms: 2
            },
            &[]
        )
        .is_err());
    assert_eq!(runtime.snapshot().phase, SessionPhase::Stopped);
    assert!(all_auto(&runtime.snapshot()));
}

#[test]
fn fresh_constructor_does_not_claim_or_change_preexisting_manual_hardware() {
    let clock = FakeClock(Rc::new(RefCell::new(20)));
    let device = Rc::new(RefCell::new(Device::default()));
    device.borrow_mut().states.insert(
        FanId(0),
        HardwareState {
            mode: Mode::Manual,
            target_rpm: Some(2000.0),
        },
    );
    let mut runtime = Coordinator::new(
        FakeActuator {
            device: device.clone(),
            clock: clock.clone(),
        },
        Tokens(0),
        clock,
    );
    let snapshot = runtime.watchdog().unwrap();
    assert_eq!(snapshot.phase, SessionPhase::Idle);
    assert!(snapshot.fans.is_empty());
    assert!(device.borrow().writes.is_empty());
    assert_eq!(device.borrow().states[&FanId(0)].mode, Mode::Manual);
    assert!(runtime.start(owner(), plan(vec![fixed(0)]), &[]).is_err());
    assert_eq!(runtime.snapshot().fans[0].1, FanOutcome::Yielded);
    assert!(device.borrow().writes.is_empty());
}

fn short_timeout_sensor(fan: u16) -> FanPlan {
    let mut entry = sensor(fan);
    if let Strategy::Sensor { config, .. } = &mut entry.strategy {
        config.sample_timeout_ms = 10;
    }
    entry
}

#[test]
fn watchdog_alone_enforces_sensor_timeout_boundary_for_whole_preset() {
    let (mut r, d, c) = fixture();
    r.start(
        owner(),
        plan(vec![fixed(0), short_timeout_sensor(1)]),
        &sample(60.0, 0),
    )
    .unwrap();
    *c.0.borrow_mut() = 10;
    assert_eq!(r.watchdog().unwrap().phase, SessionPhase::Running);
    assert!(d
        .borrow()
        .states
        .iter()
        .filter(|(fan, _)| fan.0 < 2)
        .all(|(_, state)| state.mode == Mode::Manual));
    *c.0.borrow_mut() = 11;
    assert_eq!(
        r.watchdog(),
        Err(macs_fan_control_runtime::Error::Sensor {
            sensor: SensorId("cpu.package".into()),
            fault: policy::Fault::Expired
        })
    );
    assert!(all_auto(&r.snapshot()));
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    let writes = d.borrow().writes.len();
    *c.0.borrow_mut() = 12;
    r.step(&sample(60.0, 12)).unwrap();
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    assert_eq!(d.borrow().writes.len(), writes);
}

#[test]
fn watchdog_sensor_timeout_uses_post_poll_clock() {
    let (mut r, d, c) = fixture();
    r.start(
        owner(),
        plan(vec![fixed(0), short_timeout_sensor(1)]),
        &sample(60.0, 0),
    )
    .unwrap();
    *c.0.borrow_mut() = 5;
    d.borrow_mut().advance_on_read = 6;
    assert_eq!(
        r.watchdog(),
        Err(macs_fan_control_runtime::Error::Sensor {
            sensor: SensorId("cpu.package".into()),
            fault: policy::Fault::Expired
        })
    );
    assert_eq!(*c.0.borrow(), 11);
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    assert!(all_auto(&r.snapshot()));
}

#[test]
fn watchdog_stops_expired_preparing_session_without_writes() {
    let (mut r, d, c) = fixture();
    let mut entry = short_timeout_sensor(1);
    if let Strategy::Sensor { config, .. } = &mut entry.strategy {
        config.recovery_samples = 2;
    }
    assert_eq!(
        r.start(owner(), plan(vec![fixed(0), entry]), &sample(60.0, 0))
            .unwrap()
            .phase,
        SessionPhase::Preparing
    );
    *c.0.borrow_mut() = 11;
    assert!(r.watchdog().is_err());
    assert_eq!(r.snapshot().phase, SessionPhase::Stopped);
    assert!(d.borrow().writes.is_empty());
}

#[test]
fn watchdog_fixed_only_session_remains_governed_by_lease_ttl() {
    let (mut r, _, c) = fixture();
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    *c.0.borrow_mut() = 11;
    assert_eq!(r.watchdog().unwrap().phase, SessionPhase::Running);
    *c.0.borrow_mut() = 4999;
    assert_eq!(r.watchdog().unwrap().phase, SessionPhase::Running);
    *c.0.borrow_mut() = 5000;
    assert!(r.watchdog().is_err());
    assert!(all_auto(&r.snapshot()));
}

fn raw_acquire(r: &mut Runtime, who: Owner, fan: u16) -> LeaseGrant {
    match r
        .execute_direct(
            who,
            Command::Acquire {
                fan: FanId(fan),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1,
            },
        )
        .unwrap()
    {
        Response::Lease(grant) => grant,
        response => panic!("{response:?}"),
    }
}

#[test]
fn raw_and_plan_use_one_visible_ledger_and_cannot_overlap() {
    let (mut r, d, _) = fixture();
    let grant = raw_acquire(&mut r, owner(), 0);
    assert_eq!(r.snapshot().phase, SessionPhase::Idle);
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::Active { .. }
    ));
    assert_eq!(r.statuses().len(), 1);
    assert_eq!(
        r.start(owner(), plan(vec![fixed(1)]), &[]),
        Err(macs_fan_control_runtime::Error::Busy)
    );
    r.execute_direct(
        owner(),
        Command::Release {
            fan: FanId(0),
            token: grant.token,
            seq: 2,
        },
    )
    .unwrap();
    r.start(owner(), plan(vec![fixed(1)]), &[]).unwrap();
    let writes = d.borrow().writes.len();
    assert_eq!(
        r.execute_direct(
            owner(),
            Command::Acquire {
                fan: FanId(2),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1
            }
        ),
        Err(macs_fan_control_runtime::Error::Busy)
    );
    assert_eq!(d.borrow().writes.len(), writes);
    assert!(matches!(
        r.execute_direct(owner(), Command::Enumerate),
        Ok(Response::Fans(_))
    ));
    assert!(matches!(
        r.execute_direct(owner(), Command::Capabilities { fan: FanId(1) }),
        Ok(Response::Capabilities(_))
    ));
    assert_eq!(r.snapshot().phase, SessionPhase::Running);
    r.stop(StopReason::Explicit).unwrap();
    raw_acquire(&mut r, owner(), 2);
    assert_eq!(r.snapshot().phase, SessionPhase::Idle);
}

#[test]
fn direct_tick_is_rejected_without_lifecycle_access() {
    let (mut r, _, _) = fixture();
    for tick in [
        TickReason::Periodic,
        TickReason::Sleep,
        TickReason::Shutdown,
        TickReason::Disconnected(owner()),
        TickReason::ControlFault(FanId(0)),
    ] {
        assert_eq!(
            r.execute_direct(owner(), Command::Tick(tick)),
            Err(macs_fan_control_runtime::Error::InvalidDirectCommand)
        );
    }
    raw_acquire(&mut r, owner(), 0);
}

#[test]
fn unsafe_stopped_raw_state_blocks_plan_restart_and_direct_mutation() {
    let (mut r, d, _) = fixture();
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    d.borrow_mut().fail_auto = true;
    r.stop(StopReason::Explicit).unwrap();
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::Recovering { .. }
    ));
    assert_eq!(
        r.start(owner(), plan(vec![fixed(1)]), &[]),
        Err(macs_fan_control_runtime::Error::Busy)
    );
    assert_eq!(
        r.execute_direct(
            owner(),
            Command::Acquire {
                fan: FanId(1),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1
            }
        ),
        Err(macs_fan_control_runtime::Error::Busy)
    );
}

#[test]
fn raw_recovery_and_yielded_ledger_block_plan_start() {
    for yielded in [false, true] {
        let (mut r, d, _) = fixture();
        raw_acquire(&mut r, owner(), 0);
        if yielded {
            d.borrow_mut().states.get_mut(&FanId(0)).unwrap().target_rpm = Some(2200.0);
            r.watchdog().unwrap();
            assert!(matches!(
                r.status(FanId(0)).unwrap().phase,
                Phase::Yielded { .. }
            ));
        } else {
            d.borrow_mut().fail_auto = true;
            r.disconnect(owner()).unwrap();
        }
        assert_eq!(
            r.start(owner(), plan(vec![fixed(1)]), &[]),
            Err(macs_fan_control_runtime::Error::Busy)
        );
    }
}

#[test]
fn watchdog_without_plan_audits_expires_and_retries_raw_leases() {
    let (mut r, d, c) = fixture();
    raw_acquire(&mut r, owner(), 0);
    d.borrow_mut().fail_auto = true;
    *c.0.borrow_mut() = 5000;
    r.watchdog().unwrap();
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::Recovering { .. }
    ));
    d.borrow_mut().fail_auto = false;
    *c.0.borrow_mut() += RECOVERY_RETRY_MS;
    r.watchdog().unwrap();
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::AutomaticConfirmed { .. }
    ));
    assert_eq!(r.snapshot().phase, SessionPhase::Idle);
}

#[test]
fn terminal_events_without_plan_suspend_raw_control_for_all_owners() {
    for reason in [StopReason::Sleep, StopReason::Shutdown] {
        let (mut r, _, _) = fixture();
        raw_acquire(&mut r, owner(), 0);
        raw_acquire(&mut r, Owner::from_authenticated_transport([92; 32]), 1);
        r.stop(reason).unwrap();
        assert!(r
            .statuses()
            .iter()
            .all(|s| matches!(s.phase, Phase::AutomaticConfirmed { .. })));
        assert_eq!(
            r.execute_direct(
                owner(),
                Command::Acquire {
                    fan: FanId(0),
                    rpm: 2000.0,
                    ttl_ms: 5000,
                    seq: 1
                }
            ),
            Err(macs_fan_control_runtime::Error::Core(
                macs_fan_control_core::Error::Suspended
            ))
        );
        assert!(matches!(
            r.execute_direct(owner(), Command::Enumerate),
            Ok(Response::Fans(_))
        ));
    }
}

#[test]
fn disconnect_is_owner_scoped_for_plan_and_always_revokes_raw_owner() {
    let (mut r, _, _) = fixture();
    let other = Owner::from_authenticated_transport([92; 32]);
    raw_acquire(&mut r, owner(), 0);
    raw_acquire(&mut r, other, 1);
    r.disconnect(owner()).unwrap();
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::AutomaticConfirmed { .. }
    ));
    assert!(matches!(
        r.status(FanId(1)).unwrap().phase,
        Phase::Active { .. }
    ));
    r.disconnect(other).unwrap();
    r.start(other, plan(vec![fixed(1)]), &[]).unwrap();
    assert_eq!(r.disconnect(owner()).unwrap().phase, SessionPhase::Running);
    assert!(matches!(
        r.status(FanId(1)).unwrap().phase,
        Phase::Active { .. }
    ));
    assert_eq!(r.disconnect(other).unwrap().phase, SessionPhase::Stopped);
    assert!(matches!(
        r.status(FanId(1)).unwrap().phase,
        Phase::AutomaticConfirmed { .. }
    ));
}

#[test]
fn raw_clock_rollback_revokes_every_owner_and_blocks_future_acquire() {
    let (mut r, _, c) = fixture();
    *c.0.borrow_mut() = 100;
    raw_acquire(&mut r, owner(), 0);
    raw_acquire(&mut r, Owner::from_authenticated_transport([92; 32]), 1);
    *c.0.borrow_mut() = 50;
    assert_eq!(
        r.watchdog(),
        Err(macs_fan_control_runtime::Error::Core(
            macs_fan_control_core::Error::ClockRollback
        ))
    );
    assert!(r.statuses().iter().all(|s| matches!(
        s.phase,
        Phase::AutomaticConfirmed {
            last_fault: Some(Fault::ClockRollback)
        }
    )));
    assert_eq!(
        r.execute_direct(
            owner(),
            Command::Acquire {
                fan: FanId(0),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1
            }
        ),
        Err(macs_fan_control_runtime::Error::Core(
            macs_fan_control_core::Error::ClockUntrusted
        ))
    );
}

#[test]
fn direct_slow_io_cannot_publish_expired_acquire_or_target_grant() {
    for update in [false, true] {
        let (mut r, d, _) = fixture();
        let grant = update.then(|| raw_acquire(&mut r, owner(), 0));
        d.borrow_mut().advance_on_target = 5001;
        let command = match grant {
            Some(grant) => Command::SetTarget {
                fan: FanId(0),
                token: grant.token,
                rpm: 2200.0,
                ttl_ms: 5000,
                seq: 2,
            },
            None => Command::Acquire {
                fan: FanId(0),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1,
            },
        };
        assert_eq!(
            r.execute_direct(owner(), command),
            Err(macs_fan_control_runtime::Error::SessionLost(FanId(0)))
        );
        assert!(matches!(
            r.status(FanId(0)).unwrap().phase,
            Phase::AutomaticConfirmed { .. }
        ));
    }
}

#[test]
fn direct_commands_after_safely_stopped_sensor_plan_ignore_old_sample() {
    let (mut r, _, c) = fixture();
    r.start(owner(), plan(vec![sensor(0)]), &sample(40.0, 0))
        .unwrap();
    r.stop(StopReason::Explicit).unwrap();
    *c.0.borrow_mut() = 2000;
    raw_acquire(&mut r, owner(), 1);
}

#[test]
fn restored_plan_can_be_retired_for_raw_control_on_its_former_fan() {
    let (mut r, _, _) = fixture();
    r.start(owner(), plan(vec![fixed(0)]), &[]).unwrap();
    r.stop(StopReason::Explicit).unwrap();
    let grant = raw_acquire(&mut r, owner(), 0);
    assert_eq!(r.snapshot().phase, SessionPhase::Idle);
    let grant = match r
        .execute_direct(
            owner(),
            Command::Renew {
                fan: FanId(0),
                token: grant.token,
                ttl_ms: 5000,
                seq: 2,
            },
        )
        .unwrap()
    {
        Response::Lease(grant) => grant,
        response => panic!("{response:?}"),
    };
    // Explicit plan stop never revokes direct leases, even with a retired plan.
    r.stop(StopReason::Explicit).unwrap();
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::Active { .. }
    ));
    r.execute_direct(
        owner(),
        Command::Release {
            fan: FanId(0),
            token: grant.token,
            seq: 3,
        },
    )
    .unwrap();
}

#[test]
fn all_direct_mutations_are_blocked_during_preparing_and_running_plans() {
    for preparing in [false, true] {
        let (mut r, _, _) = fixture();
        let entry = if preparing {
            let mut entry = sensor(0);
            if let Strategy::Sensor { config, .. } = &mut entry.strategy {
                config.recovery_samples = 2;
            }
            entry
        } else {
            fixed(0)
        };
        let snapshot = r
            .start(owner(), plan(vec![entry]), &sample(40.0, 0))
            .unwrap();
        for command in [
            Command::Acquire {
                fan: FanId(1),
                rpm: 2000.0,
                ttl_ms: 5000,
                seq: 1,
            },
            Command::Renew {
                fan: FanId(0),
                token: LeaseToken::from_transport_bytes([1; 32]),
                ttl_ms: 5000,
                seq: 2,
            },
            Command::SetTarget {
                fan: FanId(0),
                token: LeaseToken::from_transport_bytes([1; 32]),
                rpm: 2200.0,
                ttl_ms: 5000,
                seq: 2,
            },
            Command::Release {
                fan: FanId(0),
                token: LeaseToken::from_transport_bytes([1; 32]),
                seq: 2,
            },
        ] {
            assert_eq!(
                r.execute_direct(owner(), command),
                Err(macs_fan_control_runtime::Error::Busy)
            );
            assert_eq!(r.snapshot(), snapshot);
        }
    }
}

#[test]
fn direct_slow_renew_rejects_expired_grant_and_recovers() {
    let (mut r, d, _) = fixture();
    let grant = raw_acquire(&mut r, owner(), 0);
    d.borrow_mut().advance_on_read = 5001;
    assert_eq!(
        r.execute_direct(
            owner(),
            Command::Renew {
                fan: FanId(0),
                token: grant.token,
                ttl_ms: 5000,
                seq: 2
            }
        ),
        Err(macs_fan_control_runtime::Error::SessionLost(FanId(0)))
    );
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::AutomaticConfirmed { .. }
    ));
}

#[test]
fn rollback_after_io_and_between_core_and_runtime_watermarks_revokes_raw() {
    for during_io in [false, true] {
        let (mut r, d, c) = fixture();
        *c.0.borrow_mut() = 100;
        if during_io {
            d.borrow_mut().clock_on_target = Some(99);
            assert_eq!(
                r.execute_direct(
                    owner(),
                    Command::Acquire {
                        fan: FanId(0),
                        rpm: 2000.0,
                        ttl_ms: 5000,
                        seq: 1
                    }
                ),
                Err(macs_fan_control_runtime::Error::Core(
                    macs_fan_control_core::Error::ClockRollback
                ))
            );
        } else {
            d.borrow_mut().advance_on_target = 10;
            raw_acquire(&mut r, owner(), 0);
            *c.0.borrow_mut() = 105; // core observed 100, runtime observed 110.
            assert_eq!(
                r.execute_direct(owner(), Command::Enumerate),
                Err(macs_fan_control_runtime::Error::Core(
                    macs_fan_control_core::Error::ClockRollback
                ))
            );
        }
        assert!(matches!(
            r.status(FanId(0)).unwrap().phase,
            Phase::AutomaticConfirmed {
                last_fault: Some(Fault::ClockRollback)
            }
        ));
    }
}

#[test]
fn unrelated_expiry_does_not_swallow_a_successful_new_raw_grant() {
    let (mut r, d, _) = fixture();
    r.execute_direct(
        owner(),
        Command::Acquire {
            fan: FanId(0),
            rpm: 2000.0,
            ttl_ms: 10,
            seq: 1,
        },
    )
    .unwrap();
    d.borrow_mut().advance_on_target = 20;
    let other = Owner::from_authenticated_transport([92; 32]);
    let grant = raw_acquire(&mut r, other, 1);
    assert!(matches!(
        r.status(FanId(0)).unwrap().phase,
        Phase::AutomaticConfirmed {
            last_fault: Some(Fault::Expired)
        }
    ));
    assert!(matches!(
        r.status(FanId(1)).unwrap().phase,
        Phase::Active {
            expires_at_ms: 5000,
            ..
        }
    ));
    assert_eq!(grant.fan, FanId(1));
    // Caller received a usable token rather than an error orphaning manual mode.
    r.execute_direct(
        other,
        Command::Release {
            fan: FanId(1),
            token: grant.token,
            seq: 2,
        },
    )
    .unwrap();
    assert_eq!(d.borrow().states[&FanId(1)].mode, Mode::Automatic);
}
