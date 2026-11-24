//! Host-local policy execution. No IPC decoder, OS access, hardware adapter, or unsafe code.
//! The authenticated host must serialize calls and schedule `watchdog` independently
//! of UI traffic and sampling. Constructing a coordinator never applies preferences.
#![forbid(unsafe_code)]

pub mod policy;

use macs_fan_control_core::{
    Actuator, Capabilities, Command, Engine, Error as CoreError, FanId, FanStatus, LeaseGrant,
    Owner, Phase, Response, TickReason, TokenSource, MAX_TTL_MS,
};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_PLAN_FANS: usize = 16;
const MAX_SENSOR_HISTORY: usize = 256;

/// Trusted host monotonic clock, in the same epoch as sensor timestamps.
/// Refreshing around I/O prevents a blocking adapter from returning stale grants.
pub trait Clock {
    fn now_ms(&mut self) -> u64;
}

/// Stable sensor identifier assigned by the trusted sampler, never a display label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SensorId(pub String);

#[derive(Clone, Debug)]
pub struct SensorReading {
    pub sensor: SensorId,
    pub sample: policy::Sample,
}

#[derive(Clone, Debug)]
pub enum Strategy {
    /// Observe only: never take ownership to force somebody else's fan automatic.
    Automatic,
    Fixed {
        rpm: f64,
    },
    Sensor {
        sensor: SensorId,
        config: policy::Config,
    },
}
#[derive(Clone, Debug)]
pub struct FanPlan {
    pub fan: FanId,
    pub strategy: Strategy,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub entries: Vec<FanPlan>,
    pub ttl_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    Busy,
    InvalidPlan,
    InvalidDirectCommand,
    InvalidSensorId,
    DuplicateSample,
    InvalidConfig(policy::ConfigError),
    Sensor {
        sensor: SensorId,
        fault: policy::Fault,
    },
    Core(CoreError),
    SessionLost(FanId),
    SequenceExhausted,
    UnexpectedResponse,
}
impl From<CoreError> for Error {
    fn from(value: CoreError) -> Self {
        Self::Core(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Explicit,
    Disconnected,
    Sleep,
    Shutdown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    Preparing,
    Running,
    Stopped,
}
#[derive(Clone, Debug, PartialEq)]
pub enum FanOutcome {
    /// No control-engine readback exists. This is not confirmed automatic mode.
    Unobserved,
    Active {
        target_rpm: f64,
        expires_at_ms: u64,
    },
    AutomaticConfirmed,
    RecoveryPending,
    RecoveryExhausted,
    /// Ownership was relinquished to an observed external controller.
    Yielded,
    /// A transaction was attempted but no authoritative engine phase exists.
    Unknown,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub phase: SessionPhase,
    pub fans: Vec<(FanId, FanOutcome)>,
    pub last_error: Option<Error>,
    pub stop_reason: Option<StopReason>,
}

struct Entry {
    plan: FanPlan,
    caps: Capabilities,
    policy: Option<policy::Policy>,
    target: Option<f64>,
    grant: Option<LeaseGrant>,
    attempted: bool,
    sampled_at: Option<u64>,
}
struct Session {
    owner: Owner,
    ttl_ms: u64,
    entries: Vec<Entry>,
    phase: SessionPhase,
    last_error: Option<Error>,
    stop_reason: Option<StopReason>,
}

pub struct Coordinator<A, T, C> {
    clock: C,
    engine: Engine<A, T>,
    session: Option<Session>,
    last_now: Option<u64>,
    // Bounded cross-session replay history. A fresh host is required after 256 IDs.
    watermarks: BTreeMap<SensorId, u64>,
    terminal: bool,
}

impl<A: Actuator, T: TokenSource, C: Clock> Coordinator<A, T, C> {
    /// Starts a fresh engine ledger, without observing or changing physical mode.
    /// An existing engine cannot be handed off: its leases/recovery must remain
    /// with its current owner until resolved. No existing ledger is discarded.
    ///
    /// ```compile_fail
    /// use macs_fan_control_core::{Actuator, Engine, TokenSource};
    /// use macs_fan_control_runtime::{Clock, Coordinator};
    /// fn cannot_hide_existing_leases<A: Actuator, T: TokenSource, C: Clock>(
    ///     engine: Engine<A, T>, clock: C,
    /// ) {
    ///     let _ = Coordinator::new(engine, clock);
    /// }
    /// ```
    pub fn new(actuator: A, tokens: T, clock: C) -> Self {
        Self {
            engine: Engine::new(actuator, tokens),
            clock,
            session: None,
            last_now: None,
            watermarks: BTreeMap::new(),
            terminal: false,
        }
    }

    /// Latest evidence from the single shared engine ledger; performs no I/O.
    pub fn statuses(&self) -> Vec<FanStatus> {
        self.engine.statuses()
    }

    pub fn status(&self, fan: FanId) -> Option<FanStatus> {
        self.engine.status(fan)
    }

    /// Authenticated direct control shares the plan ledger and trusted clock.
    /// Lifecycle ticks have separate host-only entry points. Read-only queries
    /// remain available during plans; mutation requires no plan or safe stop.
    pub fn execute_direct(&mut self, owner: Owner, command: Command) -> Result<Response, Error> {
        if matches!(command, Command::Tick(_)) {
            return Err(Error::InvalidDirectCommand);
        }
        let now = self.clock.now_ms();
        self.check_time(now)?;
        if !matches!(command, Command::Enumerate | Command::Capabilities { .. }) {
            if !self.plan_safely_stopped() {
                return Err(Error::Busy);
            }
            if !self.terminal {
                // A fully restored historical plan no longer owns anything. Retire
                // it before direct writes so its old fan IDs cannot mistake raw
                // leases for unresolved plan attempts and block their release.
                self.session = None;
            }
        }
        self.execute(owner, command)
    }

    fn plan_safely_stopped(&self) -> bool {
        self.session.as_ref().is_none_or(|s| {
            s.phase == SessionPhase::Stopped
                && s.entries.iter().all(|e| {
                    !e.attempted
                        || matches!(
                            self.engine.status(e.plan.fan).map(|s| s.phase),
                            Some(Phase::AutomaticConfirmed { .. })
                        )
                })
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let Some(session) = &self.session else {
            return Snapshot {
                phase: SessionPhase::Idle,
                fans: vec![],
                last_error: None,
                stop_reason: None,
            };
        };
        Snapshot {
            phase: session.phase,
            fans: session
                .entries
                .iter()
                .map(|entry| {
                    let outcome = match self.engine.status(entry.plan.fan).map(|s| s.phase) {
                        Some(Phase::Active {
                            target_rpm,
                            expires_at_ms,
                            ..
                        }) => FanOutcome::Active {
                            target_rpm,
                            expires_at_ms,
                        },
                        Some(Phase::AutomaticConfirmed { .. }) => FanOutcome::AutomaticConfirmed,
                        Some(Phase::Recovering {
                            exhausted: false, ..
                        }) => FanOutcome::RecoveryPending,
                        Some(Phase::Recovering {
                            exhausted: true, ..
                        }) => FanOutcome::RecoveryExhausted,
                        Some(Phase::Yielded { .. }) => FanOutcome::Yielded,
                        None if entry.attempted => FanOutcome::Unknown,
                        None => FanOutcome::Unobserved,
                    };
                    (entry.plan.fan, outcome)
                })
                .collect(),
            last_error: session.last_error.clone(),
            stop_reason: session.stop_reason,
        }
    }

    /// Explicit host consent only. Whole-plan validation precedes the first Acquire.
    /// Preparing collects the configured number of fresh samples without any writes.
    pub fn start(
        &mut self,
        owner: Owner,
        plan: Plan,
        samples: &[SensorReading],
    ) -> Result<Snapshot, Error> {
        let now = self.clock.now_ms();
        self.check_time(now)?;
        if self.terminal {
            return Err(CoreError::Suspended.into());
        }
        if !self.plan_safely_stopped()
            || self
                .engine
                .statuses()
                .iter()
                .any(|s| !matches!(s.phase, Phase::AutomaticConfirmed { .. }))
        {
            return Err(Error::Busy);
        }
        if plan.entries.is_empty() || plan.entries.len() > MAX_PLAN_FANS {
            return Err(Error::InvalidPlan);
        }
        Self::deadline(now, plan.ttl_ms)?;
        let mut seen = BTreeSet::new();
        for e in &plan.entries {
            if !seen.insert(e.fan) {
                return Err(Error::InvalidPlan);
            }
        }
        let inventory = match self.execute(owner, Command::Enumerate)? {
            Response::Fans(fans) => fans,
            _ => return Err(Error::UnexpectedResponse),
        };
        let mut entries = vec![];
        for e in plan.entries {
            if !inventory.contains(&e.fan) {
                return Err(CoreError::UnknownFan.into());
            }
            let caps = match self.execute(owner, Command::Capabilities { fan: e.fan })? {
                Response::Capabilities(caps) => caps,
                _ => return Err(Error::UnexpectedResponse),
            };
            let policy = match &e.strategy {
                Strategy::Automatic => None,
                Strategy::Fixed { rpm } => {
                    // An explicit fixed target is exact, unlike a continuous sensor curve.
                    if caps.quantize_target(*rpm)? != *rpm {
                        return Err(CoreError::InvalidRpm.into());
                    }
                    None
                }
                Strategy::Sensor { sensor, config } => {
                    if sensor.0.is_empty() || sensor.0.len() > 256 {
                        return Err(Error::InvalidSensorId);
                    }
                    config.validate().map_err(Error::InvalidConfig)?;
                    if config.min_rpm < caps.min_rpm || config.max_rpm > caps.max_rpm {
                        return Err(CoreError::InvalidRpm.into());
                    }
                    // Both endpoints must be representable; all intermediate targets are
                    // quantized before any transaction in a multi-fan step.
                    if caps.quantize_target(config.min_rpm)? != config.min_rpm
                        || caps.quantize_target(config.max_rpm)? != config.max_rpm
                    {
                        return Err(CoreError::InvalidRpm.into());
                    }
                    Some(policy::Policy::new(*config).map_err(Error::InvalidConfig)?)
                }
            };
            entries.push(Entry {
                plan: e,
                caps,
                policy,
                target: None,
                grant: None,
                attempted: false,
                sampled_at: None,
            });
        }
        let mut candidate = Session {
            owner,
            ttl_ms: plan.ttl_ms,
            entries,
            phase: SessionPhase::Preparing,
            last_error: None,
            stop_reason: None,
        };
        // Preserve previous stopped session if preflight fails, and never write.
        let now = self.clock.now_ms();
        self.check_time(now)?;
        let accepted = self.evaluate(&mut candidate, samples, now)?;
        self.watermarks = accepted;
        self.session = Some(candidate);
        self.apply(now)?;
        Ok(self.snapshot())
    }

    /// Host sampling loop. Every referenced sensor must have a distinct fresh sample.
    /// Any invalid input stops the whole session; later good samples cannot restart it.
    pub fn step(&mut self, samples: &[SensorReading]) -> Result<Snapshot, Error> {
        self.watchdog()?;
        let now = self.clock.now_ms();
        self.check_time(now)?;
        let Some(mut session) = self.session.take() else {
            return Ok(self.snapshot());
        };
        if session.phase == SessionPhase::Stopped {
            self.session = Some(session);
            return Ok(self.snapshot());
        }
        let evaluated = self.evaluate(&mut session, samples, now);
        self.session = Some(session);
        match evaluated {
            Ok(accepted) => self.watermarks = accepted,
            Err(error) => {
                self.fail(error.clone(), now);
                return Err(error);
            }
        }
        self.apply(now)?;
        Ok(self.snapshot())
    }

    /// Schedule independently even if the client or sampler is hung/disconnected.
    /// Enforces stored sensor freshness even when sampling stops. Does not renew
    /// leases or resume stopped sessions. Drives bounded core recovery.
    pub fn watchdog(&mut self) -> Result<Snapshot, Error> {
        let now = self.clock.now_ms();
        self.check_time(now)?;
        let owner = self
            .session
            .as_ref()
            .map_or_else(Self::host_owner, |s| s.owner);
        if let Err(error) = self.execute(owner, Command::Tick(TickReason::Periodic)) {
            self.fail(error.clone(), self.last_now.unwrap_or(now));
            return Err(error);
        }
        if self.session.is_some() {
            // Core polling may itself block. Sampling freshness must be checked
            // using a new clock read even when no step/sample call arrives.
            let now = self.clock.now_ms();
            self.check_time(now)?;
            let s = self.session.as_ref().unwrap();
            if matches!(s.phase, SessionPhase::Preparing | SessionPhase::Running) {
                if let Err(error) = self.freshness(now) {
                    self.fail(error.clone(), now);
                    return Err(error);
                }
            }
            let s = self.session.as_ref().unwrap();
            if s.phase == SessionPhase::Running {
                if let Some(fan) = s
                    .entries
                    .iter()
                    .filter(|e| e.grant.is_some())
                    .find_map(|e| {
                        (!matches!(
                            self.engine.status(e.plan.fan).map(|s| s.phase),
                            Some(Phase::Active { .. })
                        ))
                        .then_some(e.plan.fan)
                    })
                {
                    let error = Error::SessionLost(fan);
                    self.fail(error.clone(), now);
                    return Err(error);
                }
            }
        }
        Ok(self.snapshot())
    }

    pub fn stop(&mut self, reason: StopReason) -> Result<Snapshot, Error> {
        let now = self.clock.now_ms();
        self.check_time(now)?;
        let owner = self
            .session
            .as_ref()
            .map_or_else(Self::host_owner, |s| s.owner);
        let tick = match reason {
            StopReason::Sleep => Some(TickReason::Sleep),
            StopReason::Shutdown => Some(TickReason::Shutdown),
            StopReason::Explicit | StopReason::Disconnected => self
                .session
                .as_ref()
                .filter(|s| s.phase != SessionPhase::Stopped)
                .map(|s| TickReason::Disconnected(s.owner)),
        };
        if matches!(reason, StopReason::Sleep | StopReason::Shutdown) {
            self.terminal = true;
        }
        if let Some(s) = &mut self.session {
            s.phase = SessionPhase::Stopped;
            s.stop_reason = Some(reason);
        }
        if let Some(tick) = tick {
            self.execute(owner, Command::Tick(tick))?;
        }
        Ok(self.snapshot())
    }

    /// Always revoke the disconnected owner's raw leases. Only that same
    /// owner's plan is stopped; disconnecting another connection leaves it live.
    pub fn disconnect(&mut self, owner: Owner) -> Result<Snapshot, Error> {
        let now = self.clock.now_ms();
        self.check_time(now)?;
        if let Some(s) = &mut self.session {
            if s.owner == owner {
                s.phase = SessionPhase::Stopped;
                s.stop_reason = Some(StopReason::Disconnected);
            }
        }
        self.execute(owner, Command::Tick(TickReason::Disconnected(owner)))?;
        Ok(self.snapshot())
    }

    fn host_owner() -> Owner {
        // Tick ownership is carried by TickReason; this is never a client identity.
        Owner::from_authenticated_transport([0; 32])
    }

    fn check_time(&mut self, now: u64) -> Result<(), Error> {
        if let Some(last) = self.last_now.filter(|last| now < *last) {
            self.terminal = true;
            let error = Error::Core(CoreError::ClockRollback);
            if let Some(s) = &mut self.session {
                s.phase = SessionPhase::Stopped;
                s.last_error = Some(error.clone());
            }
            // The last runtime clock read may be later than the core's last I/O
            // timestamp. Observe it before reporting the rollback, so the core
            // marks its clock untrusted and revokes EVERY owner's raw/plan lease.
            let owner = Self::host_owner();
            let _ = self
                .engine
                .execute(owner, Command::Tick(TickReason::Periodic), last);
            let _ = self
                .engine
                .execute(owner, Command::Tick(TickReason::Periodic), now);
            self.last_now = Some(now);
            return Err(error);
        }
        self.last_now = Some(now);
        Ok(())
    }

    fn deadline(now: u64, ttl: u64) -> Result<(), Error> {
        if ttl == 0 || ttl > MAX_TTL_MS {
            return Err(CoreError::InvalidTtl.into());
        }
        now.checked_add(ttl)
            .ok_or(Error::Core(CoreError::DeadlineOverflow))?;
        Ok(())
    }

    fn evaluate(
        &self,
        session: &mut Session,
        samples: &[SensorReading],
        now: u64,
    ) -> Result<BTreeMap<SensorId, u64>, Error> {
        Self::deadline(now, session.ttl_ms)?;
        // Input memory/work is bounded; sampler should provide only referenced sensors.
        if samples.len() > MAX_PLAN_FANS {
            return Err(Error::InvalidPlan);
        }
        let mut inputs = BTreeMap::new();
        for reading in samples {
            if inputs.insert(&reading.sensor, reading.sample).is_some() {
                return Err(Error::DuplicateSample);
            }
        }
        let mut accepted = self.watermarks.clone();
        for entry in &mut session.entries {
            entry.target = match &entry.plan.strategy {
                Strategy::Automatic => None,
                Strategy::Fixed { rpm } => Some(*rpm),
                Strategy::Sensor { sensor, .. } => {
                    let sample = inputs.get(sensor).copied();
                    entry.sampled_at = sample.map(|s| s.sampled_at_ms);
                    if sample.is_some_and(|s| {
                        self.watermarks
                            .get(sensor)
                            .is_some_and(|last| s.sampled_at_ms <= *last)
                    }) {
                        return Err(Error::Sensor {
                            sensor: sensor.clone(),
                            fault: policy::Fault::ReplayedSample,
                        });
                    }
                    let result = entry
                        .policy
                        .as_mut()
                        .expect("sensor policy")
                        .evaluate(sample, now);
                    match result {
                        policy::Recommendation::SuggestRpm(rpm) => {
                            accepted.insert(
                                sensor.clone(),
                                sample.expect("validated sample").sampled_at_ms,
                            );
                            Some(entry.caps.quantize_target(rpm)?)
                        }
                        policy::Recommendation::ReturnToAutomatic(policy::Fault::Recovering)
                            if session.phase == SessionPhase::Preparing =>
                        {
                            accepted.insert(
                                sensor.clone(),
                                sample.expect("validated sample").sampled_at_ms,
                            );
                            None
                        }
                        policy::Recommendation::ReturnToAutomatic(fault) => {
                            return Err(Error::Sensor {
                                sensor: sensor.clone(),
                                fault,
                            })
                        }
                    }
                }
            };
        }
        if accepted.len() > MAX_SENSOR_HISTORY {
            return Err(Error::InvalidPlan);
        }
        Ok(accepted)
    }

    fn apply(&mut self, _now: u64) -> Result<(), Error> {
        let s = self.session.as_ref().expect("session installed");
        if s.entries
            .iter()
            .any(|e| e.policy.is_some() && e.target.is_none())
        {
            return Ok(());
        }
        // Plan targets/quality were validated for every fan before this loop.
        // Engine independently revalidates capabilities and hardware before each write.
        let count = s.entries.len();
        for index in 0..count {
            let now = self.clock.now_ms();
            self.check_time(now)?;
            if let Err(error) = self.freshness(now) {
                self.fail(error.clone(), now);
                return Err(error);
            }
            let (owner, command) = {
                let s = self.session.as_mut().expect("session installed");
                let entry = &mut s.entries[index];
                let Some(rpm) = entry.target else {
                    continue;
                };
                let command = match &entry.grant {
                    None => Command::Acquire {
                        fan: entry.plan.fan,
                        rpm,
                        ttl_ms: s.ttl_ms,
                        seq: 1,
                    },
                    Some(grant) => {
                        let Some(seq) = grant.seq.checked_add(1) else {
                            self.fail(Error::SequenceExhausted, now);
                            return Err(Error::SequenceExhausted);
                        };
                        if rpm == grant.target_rpm {
                            Command::Renew {
                                fan: entry.plan.fan,
                                token: grant.token.clone(),
                                ttl_ms: s.ttl_ms,
                                seq,
                            }
                        } else {
                            Command::SetTarget {
                                fan: entry.plan.fan,
                                token: grant.token.clone(),
                                rpm,
                                ttl_ms: s.ttl_ms,
                                seq,
                            }
                        }
                    }
                };
                entry.attempted = true;
                (s.owner, command)
            };
            let result = self.execute(owner, command);
            match result {
                Ok(Response::Lease(grant)) => {
                    self.session.as_mut().unwrap().entries[index].grant = Some(grant)
                }
                other => {
                    let error = other.err().unwrap_or(Error::UnexpectedResponse);
                    self.fail(error.clone(), now);
                    return Err(error);
                }
            }
        }
        let now = self.clock.now_ms();
        self.check_time(now)?;
        if let Err(error) = self.freshness(now) {
            self.fail(error.clone(), now);
            return Err(error);
        }
        self.session.as_mut().unwrap().phase = SessionPhase::Running;
        Ok(())
    }

    fn freshness(&self, now: u64) -> Result<(), Error> {
        if let Some(s) = &self.session {
            Self::deadline(now, s.ttl_ms)?;
            for e in &s.entries {
                if matches!(self.engine.status(e.plan.fan).map(|status| status.phase),
                    Some(Phase::Active { expires_at_ms, .. }) if expires_at_ms <= now)
                {
                    return Err(Error::SessionLost(e.plan.fan));
                }
                if let Strategy::Sensor { sensor, config } = &e.plan.strategy {
                    let at = e.sampled_at.ok_or_else(|| Error::Sensor {
                        sensor: sensor.clone(),
                        fault: policy::Fault::MissingSample,
                    })?;
                    let fault = if at > now {
                        Some(policy::Fault::FutureSample)
                    } else if now - at > config.sample_timeout_ms {
                        Some(policy::Fault::Expired)
                    } else {
                        None
                    };
                    if let Some(fault) = fault {
                        return Err(Error::Sensor {
                            sensor: sensor.clone(),
                            fault,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn execute(&mut self, owner: Owner, command: Command) -> Result<Response, Error> {
        let before = self.clock.now_ms();
        self.check_time(before)?;
        if matches!(
            command,
            Command::Acquire { .. } | Command::Renew { .. } | Command::SetTarget { .. }
        ) && self
            .session
            .as_ref()
            .is_some_and(|s| s.phase != SessionPhase::Stopped)
        {
            self.freshness(before)?;
        }
        let result = self
            .engine
            .execute(owner, command, before)
            .map_err(Error::Core);
        let after = self.clock.now_ms();
        self.check_time(after)?;
        // Expiry anywhere in the shared ledger must drive recovery. An unrelated
        // owner's expired lease must not swallow a still-live new grant/token.
        let expired: Vec<_> = self
            .engine
            .statuses()
            .iter()
            .filter_map(|s| match s.phase {
                Phase::Active { expires_at_ms, .. } if expires_at_ms <= after => Some(s.fan),
                _ => None,
            })
            .collect();
        let expired_plan_fan = self
            .session
            .as_ref()
            .filter(|s| matches!(s.phase, SessionPhase::Preparing | SessionPhase::Running))
            .and_then(|s| {
                s.entries.iter().find_map(|e| {
                    (e.attempted && expired.contains(&e.plan.fan)).then_some(e.plan.fan)
                })
            });
        let mut after = after;
        if !expired.is_empty() {
            let _ = self
                .engine
                .execute(owner, Command::Tick(TickReason::Periodic), after);
            // Auditing unrelated expiry may itself block. Recheck the grant at
            // the latest trusted clock rather than returning a now-stale token.
            after = self.clock.now_ms();
            self.check_time(after)?;
        }
        if let Ok(Response::Lease(grant)) = &result {
            if grant.expires_at_ms <= after
                || !matches!(
                    self.engine.status(grant.fan).map(|s| s.phase),
                    Some(Phase::Active { expires_at_ms, seq, target_rpm })
                        if expires_at_ms == grant.expires_at_ms
                            && seq == grant.seq && target_rpm == grant.target_rpm
                )
            {
                let _ = self
                    .engine
                    .execute(owner, Command::Tick(TickReason::Periodic), after);
                let after = self.clock.now_ms();
                self.check_time(after)?;
                return Err(Error::SessionLost(grant.fan));
            }
        }
        // Plans remain whole-session transactions: expiry of an earlier plan
        // fan fails this step so apply() revokes the other private plan grants.
        if let Some(fan) = expired_plan_fan {
            return Err(Error::SessionLost(fan));
        }
        result
    }

    fn fail(&mut self, error: Error, _now: u64) {
        let now = self.clock.now_ms();
        if let Some(s) = &mut self.session {
            s.phase = SessionPhase::Stopped;
            s.last_error = Some(error);
            // Revoke all this authenticated owner's active leases, including a
            // partially committed acquisition. Core respects Yielded ownership.
            // Errors remain visible through core statuses and the primary error.
            let _ = self.engine.execute(
                s.owner,
                Command::Tick(TickReason::Disconnected(s.owner)),
                now,
            );
        }
    }
}
