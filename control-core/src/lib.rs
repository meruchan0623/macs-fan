//! Serial, hardware-independent control transactions. No SMC or privilege code.
//!
//! The trusted host supplies authenticated owners, CSPRNG tokens, and time from
//! its own monotonic clock. Never deserialize an owner or timestamp from IPC.
//! A host must run `Tick` independently of clients and forward lifecycle events.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

pub const MAX_TTL_MS: u64 = 5_000;
pub const RECOVERY_ATTEMPTS: u8 = 3;
pub const RECOVERY_RETRY_MS: u64 = 250;
const MAX_ISSUED_TOKENS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FanId(pub u16);

/// Connection identity assigned only after the transport authenticates a peer.
/// This type does not authenticate callers. In particular it is not a PID.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Owner([u8; 32]);
impl Owner {
    pub fn from_authenticated_transport(identity: [u8; 32]) -> Self {
        Self(identity)
    }
}

impl fmt::Debug for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Owner([redacted])")
    }
}

/// Bearer secret. The host must keep replies private and must not log tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct LeaseToken([u8; 32]);
impl LeaseToken {
    pub fn from_transport_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn transport_bytes(&self) -> [u8; 32] {
        self.0
    }
    fn matches(&self, other: &Self) -> bool {
        // Fixed-size comparison avoids a byte-by-byte early exit.
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0, |d, (a, b)| d | (a ^ b))
            == 0
    }
}
impl fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LeaseToken([redacted])")
    }
}

/// Production implementations MUST return fresh bytes from an OS CSPRNG.
/// No production implementation is included. Repeated/zero outputs are rejected.
pub trait TokenSource {
    fn next_token(&mut self) -> Result<[u8; 32], TokenError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSupport {
    Verified,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetEncoding {
    Fpe2,
    Float32,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeControl {
    PerFan,
    VerifiedSharedMask,
    Unknown,
}

/// Populated by the actuator's own audited model table and hardware reads.
/// Clients cannot supply this structure. `validated` includes fan-count, key,
/// size, encoding, zero-RPM permission, and model-specific safety validation.
#[derive(Clone, Debug, PartialEq)]
pub struct Capabilities {
    pub fan: FanId,
    pub validated: bool,
    pub model: ModelSupport,
    pub encoding: TargetEncoding,
    pub mode_control: ModeControl,
    pub min_rpm: f64,
    pub max_rpm: f64,
    pub step_rpm: f64,
    /// The adapter can safely stage a target while firmware remains automatic.
    pub stages_target_while_automatic: bool,
}
impl Capabilities {
    /// Validate a requested RPM and round it to the nearest hardware step.
    /// The request and result must both be within the verified hardware range.
    pub fn quantize_target(&self, rpm: f64) -> Result<f64, Error> {
        self.validate(self.fan)?;
        if !rpm.is_finite() || rpm < self.min_rpm || rpm > self.max_rpm {
            return Err(Error::InvalidRpm);
        }
        let target = (rpm / self.step_rpm).round() * self.step_rpm;
        if !self.accepts(target) {
            return Err(Error::InvalidRpm);
        }
        Ok(target)
    }
    fn validate(&self, fan: FanId) -> Result<(), Error> {
        if self.fan != fan
            || !self.validated
            || self.model != ModelSupport::Verified
            || self.encoding == TargetEncoding::Unknown
            || self.mode_control == ModeControl::Unknown
            || !self.stages_target_while_automatic
            || !self.min_rpm.is_finite()
            || !self.max_rpm.is_finite()
            || !self.step_rpm.is_finite()
            || self.min_rpm < 0.0
            || self.max_rpm < self.min_rpm
            || self.step_rpm <= 0.0
            || self.step_rpm > self.max_rpm
            || !(self.max_rpm / self.step_rpm).is_finite()
            || !self.encoding_accepts(self.min_rpm)
            || !self.encoding_accepts(self.max_rpm)
            || !self.encoding_accepts(self.step_rpm)
        {
            return Err(Error::UnverifiedCapabilities);
        }
        Ok(())
    }
    fn accepts(&self, rpm: f64) -> bool {
        let units = rpm / self.step_rpm;
        rpm.is_finite()
            && rpm >= self.min_rpm
            && rpm <= self.max_rpm
            && units.is_finite()
            && units.fract() == 0.0
            && units * self.step_rpm == rpm
            && self.encoding_accepts(rpm)
    }
    fn encoding_accepts(&self, rpm: f64) -> bool {
        match self.encoding {
            TargetEncoding::Fpe2 => {
                rpm.is_finite() && (0.0..=16383.75).contains(&rpm) && (rpm * 4.0).fract() == 0.0
            }
            TargetEncoding::Float32 => rpm.is_finite() && (rpm as f32) as f64 == rpm,
            TargetEncoding::Unknown => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Automatic,
    Manual,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    Automatic,
    Manual,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HardwareState {
    pub mode: Mode,
    /// The commanded target, not actual RPM (which has physical inertia).
    pub target_rpm: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActuatorError {
    Unavailable,
    Unsupported,
    Io,
    Timeout,
}

/// All methods execute serially under the host's single engine ownership.
/// Each write reports its actual return code; a timeout is an error even when
/// the hardware may have changed. The adapter must preserve other fan bits in
/// a shared mode mask, enforce its own verified allowlist, bound I/O duration,
/// and refuse writes if hardware identity/key metadata have become unsafe.
pub trait Actuator {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError>;
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError>;
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError>;
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError>;
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError>;
}

/// No arbitrary keys, bytes, processes, paths, scripts, or policy configuration.
#[derive(Clone, Debug)]
pub enum Command {
    Enumerate,
    Capabilities {
        fan: FanId,
    },
    Acquire {
        fan: FanId,
        rpm: f64,
        ttl_ms: u64,
        seq: u64,
    },
    Renew {
        fan: FanId,
        token: LeaseToken,
        ttl_ms: u64,
        seq: u64,
    },
    /// Adjust a live owned lease, rounding to the nearest representable step.
    /// The token is preserved; success requires manual mode and target readback.
    SetTarget {
        fan: FanId,
        token: LeaseToken,
        rpm: f64,
        ttl_ms: u64,
        seq: u64,
    },
    /// Explicit return to automatic for a currently owned lease.
    Release {
        fan: FanId,
        token: LeaseToken,
        seq: u64,
    },
    Tick(TickReason),
}

/// Trusted host events, never accepted from a client IPC request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickReason {
    Periodic,
    Disconnected(Owner),
    /// Trusted sampling/policy/watchdog fault: revoke this fan immediately.
    ControlFault(FanId),
    Sleep,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LeaseGrant {
    pub fan: FanId,
    pub token: LeaseToken,
    pub target_rpm: f64,
    pub expires_at_ms: u64,
    pub seq: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Fans(Vec<FanId>),
    Capabilities(Capabilities),
    Lease(LeaseGrant),
    AutomaticConfirmed,
    Statuses(Vec<FanStatus>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Released,
    Expired,
    Disconnected,
    Sleep,
    Shutdown,
    ClockRollback,
    ControlInput,
    CapabilityChanged,
    StateRead,
    TargetWrite,
    TargetReadback,
    ModeWrite,
    ModeReadback,
    ExternalControl,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryError {
    Write(ActuatorError),
    Read(ActuatorError),
    ReadbackMismatch,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    AutomaticConfirmed {
        last_fault: Option<Fault>,
    },
    Active {
        target_rpm: f64,
        expires_at_ms: u64,
        seq: u64,
    },
    Recovering {
        cause: Fault,
        attempts: u8,
        next_retry_at_ms: Option<u64>,
        exhausted: bool,
        last_error: RecoveryError,
    },
    /// We no longer control this fan. No automatic success is implied.
    Yielded {
        cause: Fault,
        observed: HardwareState,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct FanStatus {
    pub fan: FanId,
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    UnknownFan,
    InvalidInventory,
    UnverifiedCapabilities,
    InvalidRpm,
    InvalidTtl,
    InvalidSequence,
    DeadlineOverflow,
    OwnerConflict,
    InvalidToken,
    NoActiveLease,
    RecoveryPending,
    EntropyUnavailable,
    ClockRollback,
    ClockUntrusted,
    Suspended,
    ExternalController,
    Actuator(ActuatorError),
    /// Inspect the accompanying phase: recovery may be pending or exhausted.
    Fault {
        fan: FanId,
        cause: Fault,
        phase: Phase,
    },
}

#[derive(Clone)]
struct Lease {
    owner: Owner,
    token: LeaseToken,
    caps: Capabilities,
    target: f64,
    expires_at: u64,
    seq: u64,
    committed: bool,
    // Only populated between an owned target write and its verified readback.
    // The confirmed target/sequence/deadline remain unchanged during this time.
    attempted_target: Option<f64>,
}
#[derive(Clone, Copy)]
enum RecoveryOrigin {
    CommittedLease,
    UnconfirmedTransaction,
    TargetUpdate,
}
struct Recovery {
    cause: Fault,
    origin: RecoveryOrigin,
    // For a committed lease this is its confirmed target. For an unconfirmed
    // transaction it is the first finite manual target actually observed at
    // failure, which may differ from the requested (never confirmed) target.
    baseline_target: Option<f64>,
    attempted_target: Option<f64>,
    attempts: u8,
    next_retry: Option<u64>,
    last_error: RecoveryError,
}
enum Entry {
    Active(Lease),
    Automatic(Option<Fault>),
    Recovery(Recovery),
    Yielded(HardwareState),
}

/// Call only from one serialized executor. `&mut self` prevents overlapping
/// transactions in safe Rust; do not wrap independent engines around one device.
pub struct Engine<A, T> {
    actuator: A,
    tokens: T,
    entries: BTreeMap<FanId, Entry>,
    issued_tokens: HashSet<[u8; 32]>,
    last_now: Option<u64>,
    clock_untrusted: bool,
    suspended: bool,
}

impl<A: Actuator, T: TokenSource> Engine<A, T> {
    pub fn new(actuator: A, tokens: T) -> Self {
        Self {
            actuator,
            tokens,
            entries: BTreeMap::new(),
            issued_tokens: HashSet::new(),
            last_now: None,
            clock_untrusted: false,
            suspended: false,
        }
    }

    /// `now_ms` must be read by the helper, from a shared monotonic clock epoch.
    /// Successful acquisition means mode AND target were read back, not that
    /// physical RPM has already reached the target.
    pub fn execute(
        &mut self,
        owner: Owner,
        command: Command,
        now_ms: u64,
    ) -> Result<Response, Error> {
        if self.last_now.is_some_and(|last| now_ms < last) {
            self.last_now = Some(now_ms);
            self.clock_untrusted = true;
            // Preserve the retry budget but rebase pending retries into the
            // new epoch. An old deadline must not postpone recovery forever.
            for entry in self.entries.values_mut() {
                if let Entry::Recovery(r) = entry {
                    if r.next_retry.is_some() {
                        r.next_retry = now_ms.checked_add(RECOVERY_RETRY_MS);
                    }
                }
            }
            self.revoke_matching(now_ms, Fault::ClockRollback, |_| true);
            return Err(Error::ClockRollback);
        }
        self.last_now = Some(now_ms);
        self.revoke_matching(now_ms, Fault::Expired, |lease| now_ms >= lease.expires_at);
        match command {
            Command::Enumerate => match self.inventory() {
                Ok(fans) => {
                    let missing: Vec<_> = self
                        .entries
                        .keys()
                        .copied()
                        .filter(|fan| !fans.contains(fan))
                        .collect();
                    for fan in missing {
                        self.start_recovery(fan, now_ms, Some(Fault::CapabilityChanged));
                    }
                    Ok(Response::Fans(fans))
                }
                Err(error) => {
                    self.revoke_matching(now_ms, Fault::CapabilityChanged, |_| true);
                    Err(error)
                }
            },
            Command::Capabilities { fan } => {
                let result = self.verified_caps(fan);
                if let Some(Entry::Active(lease)) = self.entries.get(&fan) {
                    if result.as_ref().map_or(true, |caps| *caps != lease.caps) {
                        return self.fail(fan, now_ms, Fault::CapabilityChanged);
                    }
                }
                result.map(Response::Capabilities)
            }
            Command::Acquire {
                fan,
                rpm,
                ttl_ms,
                seq,
            } => {
                self.ensure_running()?;
                self.acquire(owner, fan, rpm, ttl_ms, seq, now_ms)
            }
            Command::Renew {
                fan,
                token,
                ttl_ms,
                seq,
            } => {
                self.ensure_running()?;
                self.renew(owner, fan, &token, ttl_ms, seq, now_ms)
            }
            Command::SetTarget {
                fan,
                token,
                rpm,
                ttl_ms,
                seq,
            } => {
                self.ensure_running()?;
                self.set_target(owner, fan, &token, rpm, ttl_ms, seq, now_ms)
            }
            Command::Release { fan, token, seq } => {
                self.authorize(owner, fan, &token, seq)?;
                self.check_active(fan, now_ms)?;
                self.start_recovery(fan, now_ms, None);
                match self.status(fan).expect("release has tracked fan").phase {
                    Phase::AutomaticConfirmed { .. } => Ok(Response::AutomaticConfirmed),
                    phase => Err(Error::Fault {
                        fan,
                        cause: Fault::Released,
                        phase,
                    }),
                }
            }
            Command::Tick(reason) => {
                match reason {
                    TickReason::Periodic => {}
                    TickReason::Disconnected(disconnected) => {
                        self.revoke_matching(now_ms, Fault::Disconnected, |l| {
                            l.owner == disconnected
                        })
                    }
                    TickReason::ControlFault(fan) => {
                        self.start_recovery(fan, now_ms, Some(Fault::ControlInput));
                    }
                    TickReason::Sleep | TickReason::Shutdown => {
                        // Rebuild the engine after wake/restart, revalidate hardware,
                        // and require a new explicitly consented control session.
                        self.suspended = true;
                        let cause = if reason == TickReason::Sleep {
                            Fault::Sleep
                        } else {
                            Fault::Shutdown
                        };
                        self.revoke_matching(now_ms, cause, |_| true);
                    }
                }
                self.poll(now_ms);
                Ok(Response::Statuses(self.statuses()))
            }
        }
    }

    pub fn status(&self, fan: FanId) -> Option<FanStatus> {
        self.entries.get(&fan).map(|entry| FanStatus {
            fan,
            phase: match entry {
                Entry::Active(l) => Phase::Active {
                    target_rpm: l.target,
                    expires_at_ms: l.expires_at,
                    seq: l.seq,
                },
                Entry::Automatic(cause) => Phase::AutomaticConfirmed { last_fault: *cause },
                Entry::Recovery(r) => Phase::Recovering {
                    cause: r.cause,
                    attempts: r.attempts,
                    next_retry_at_ms: r.next_retry,
                    exhausted: r.next_retry.is_none(),
                    last_error: r.last_error,
                },
                Entry::Yielded(observed) => Phase::Yielded {
                    cause: Fault::ExternalControl,
                    observed: *observed,
                },
            },
        })
    }

    pub fn statuses(&self) -> Vec<FanStatus> {
        self.entries
            .keys()
            .filter_map(|fan| self.status(*fan))
            .collect()
    }

    fn ensure_running(&self) -> Result<(), Error> {
        if self.clock_untrusted {
            Err(Error::ClockUntrusted)
        } else if self.suspended {
            Err(Error::Suspended)
        } else {
            Ok(())
        }
    }
    fn inventory(&mut self) -> Result<Vec<FanId>, Error> {
        let mut fans = self.actuator.enumerate().map_err(Error::Actuator)?;
        fans.sort_unstable();
        if fans.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::InvalidInventory);
        }
        Ok(fans)
    }
    fn verified_caps(&mut self, fan: FanId) -> Result<Capabilities, Error> {
        if !self.inventory()?.contains(&fan) {
            return Err(Error::UnknownFan);
        }
        let caps = self.actuator.capabilities(fan).map_err(Error::Actuator)?;
        caps.validate(fan)?;
        Ok(caps)
    }
    fn deadline(now: u64, ttl: u64) -> Result<u64, Error> {
        if ttl == 0 || ttl > MAX_TTL_MS {
            return Err(Error::InvalidTtl);
        }
        now.checked_add(ttl).ok_or(Error::DeadlineOverflow)
    }
    fn authorize(
        &self,
        owner: Owner,
        fan: FanId,
        token: &LeaseToken,
        seq: u64,
    ) -> Result<Lease, Error> {
        match self.entries.get(&fan) {
            Some(Entry::Active(l)) => {
                if l.owner != owner {
                    return Err(Error::OwnerConflict);
                }
                if !l.token.matches(token) {
                    return Err(Error::InvalidToken);
                }
                if seq <= l.seq {
                    return Err(Error::InvalidSequence);
                }
                Ok(l.clone())
            }
            Some(Entry::Recovery(_)) => Err(Error::RecoveryPending),
            _ => Err(Error::NoActiveLease),
        }
    }
    fn acquire(
        &mut self,
        owner: Owner,
        fan: FanId,
        rpm: f64,
        ttl: u64,
        seq: u64,
        now: u64,
    ) -> Result<Response, Error> {
        let expires_at = Self::deadline(now, ttl)?;
        if seq == 0 {
            return Err(Error::InvalidSequence);
        }
        match self.entries.get(&fan) {
            Some(Entry::Active(_)) => return Err(Error::OwnerConflict),
            Some(Entry::Recovery(_)) => return Err(Error::RecoveryPending),
            _ => {}
        }
        let caps = self.verified_caps(fan)?;
        if !caps.accepts(rpm) {
            return Err(Error::InvalidRpm);
        }
        let before = self.actuator.read_state(fan).map_err(Error::Actuator)?;
        match before.mode {
            Mode::Automatic => {}
            Mode::Manual => {
                self.entries.insert(fan, Entry::Yielded(before));
                return Err(Error::ExternalController);
            }
            Mode::Unknown => return Err(Error::UnverifiedCapabilities),
        }
        if self.issued_tokens.len() >= MAX_ISSUED_TOKENS {
            return Err(Error::EntropyUnavailable);
        }
        let bytes = self
            .tokens
            .next_token()
            .map_err(|_| Error::EntropyUnavailable)?;
        if bytes == [0; 32] || !self.issued_tokens.insert(bytes) {
            return Err(Error::EntropyUnavailable);
        }
        let lease = Lease {
            owner,
            token: LeaseToken(bytes),
            caps,
            target: rpm,
            expires_at,
            seq,
            committed: false,
            attempted_target: None,
        };
        // Track before the first write, including writes that report failure but
        // may already have changed hardware. Every partial failure is recovered.
        self.entries.insert(fan, Entry::Active(lease.clone()));
        if self.actuator.write_target(fan, rpm).is_err() {
            return self.fail(fan, now, Fault::TargetWrite);
        }
        match self.actuator.read_state(fan) {
            Ok(s) if s.mode == Mode::Automatic && s.target_rpm == Some(rpm) => {}
            Ok(s) if s.mode == Mode::Manual => {
                // We have not enabled manual mode: another actor did so.
                self.entries.insert(fan, Entry::Yielded(s));
                return Err(Error::Fault {
                    fan,
                    cause: Fault::ExternalControl,
                    phase: self.status(fan).unwrap().phase,
                });
            }
            observed => return self.fail_observed(fan, now, Fault::TargetReadback, observed.ok()),
        }
        if self.actuator.write_mode(fan, WriteMode::Manual).is_err() {
            return self.fail(fan, now, Fault::ModeWrite);
        }
        match self.actuator.read_state(fan) {
            Ok(s) if s.mode == Mode::Manual && s.target_rpm == Some(rpm) => {
                let mut committed = lease.clone();
                committed.committed = true;
                self.entries.insert(fan, Entry::Active(committed));
                Ok(Response::Lease(Self::grant(fan, &lease)))
            }
            observed => self.fail_observed(fan, now, Fault::ModeReadback, observed.ok()),
        }
    }
    fn grant(fan: FanId, lease: &Lease) -> LeaseGrant {
        LeaseGrant {
            fan,
            token: lease.token.clone(),
            target_rpm: lease.target,
            expires_at_ms: lease.expires_at,
            seq: lease.seq,
        }
    }
    fn renew(
        &mut self,
        owner: Owner,
        fan: FanId,
        token: &LeaseToken,
        ttl: u64,
        seq: u64,
        now: u64,
    ) -> Result<Response, Error> {
        let expires_at = Self::deadline(now, ttl)?;
        let mut lease = self.authorize(owner, fan, token, seq)?;
        self.check_active(fan, now)?;
        // Renew never writes a target and cannot revive a faulted/revoked lease.
        lease.seq = seq;
        lease.expires_at = expires_at;
        self.entries.insert(fan, Entry::Active(lease.clone()));
        Ok(Response::Lease(Self::grant(fan, &lease)))
    }
    #[allow(clippy::too_many_arguments)]
    fn set_target(
        &mut self,
        owner: Owner,
        fan: FanId,
        token: &LeaseToken,
        rpm: f64,
        ttl: u64,
        seq: u64,
        now: u64,
    ) -> Result<Response, Error> {
        let expires_at = Self::deadline(now, ttl)?;
        let mut lease = self.authorize(owner, fan, token, seq)?;
        let target = lease.caps.quantize_target(rpm)?;
        // Revalidate both current capabilities and ownership before any write.
        self.check_active(fan, now)?;
        if target != lease.target {
            lease.attempted_target = Some(target);
            self.entries.insert(fan, Entry::Active(lease.clone()));
            if self.actuator.write_target(fan, target).is_err() {
                return self.fail(fan, now, Fault::TargetWrite);
            }
            match self.actuator.read_state(fan) {
                Ok(state) if state.mode == Mode::Manual && state.target_rpm == Some(target) => {}
                Ok(state)
                    if state.mode == Mode::Automatic
                        || Self::finite_manual_target(state).is_some_and(|observed| {
                            observed != lease.target && observed != target
                        }) =>
                {
                    self.entries.insert(fan, Entry::Yielded(state));
                    return Err(Error::Fault {
                        fan,
                        cause: Fault::ExternalControl,
                        phase: self.status(fan).unwrap().phase,
                    });
                }
                observed => {
                    return self.fail_observed(fan, now, Fault::TargetReadback, observed.ok());
                }
            }
        }
        // An alias of the confirmed target is a verified renewal without writes.
        lease.target = target;
        lease.seq = seq;
        lease.expires_at = expires_at;
        lease.attempted_target = None;
        self.entries.insert(fan, Entry::Active(lease.clone()));
        Ok(Response::Lease(Self::grant(fan, &lease)))
    }
    fn check_active(&mut self, fan: FanId, now: u64) -> Result<(), Error> {
        let Some(Entry::Active(lease)) = self.entries.get(&fan) else {
            return Ok(());
        };
        let lease = lease.clone();
        match self.verified_caps(fan) {
            Ok(caps) if caps == lease.caps => {}
            _ => return self.fail(fan, now, Fault::CapabilityChanged),
        }
        match self.actuator.read_state(fan) {
            Ok(state) if state.mode == Mode::Manual && state.target_rpm == Some(lease.target) => {
                Ok(())
            }
            Ok(state)
                if state.mode == Mode::Automatic
                    || (state.mode == Mode::Manual
                        && state.target_rpm.is_some_and(|rpm| rpm.is_finite())) =>
            {
                self.entries.insert(fan, Entry::Yielded(state));
                Err(Error::Fault {
                    fan,
                    cause: Fault::ExternalControl,
                    phase: self.status(fan).unwrap().phase,
                })
            }
            _ => self.fail(fan, now, Fault::StateRead),
        }
    }
    fn fail<R>(&mut self, fan: FanId, now: u64, cause: Fault) -> Result<R, Error> {
        self.fail_observed(fan, now, cause, None)
    }
    fn fail_observed<R>(
        &mut self,
        fan: FanId,
        now: u64,
        cause: Fault,
        observed: Option<HardwareState>,
    ) -> Result<R, Error> {
        self.start_recovery_observed(fan, now, Some(cause), observed);
        Err(Error::Fault {
            fan,
            cause,
            phase: self.status(fan).expect("fault has tracked fan").phase,
        })
    }
    fn revoke_matching(&mut self, now: u64, cause: Fault, predicate: impl Fn(&Lease) -> bool) {
        let fans: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(fan, entry)| match entry {
                Entry::Active(l) if predicate(l) => Some(*fan),
                _ => None,
            })
            .collect();
        for fan in fans {
            self.start_recovery(fan, now, Some(cause));
        }
    }
    fn start_recovery(&mut self, fan: FanId, now: u64, cause: Option<Fault>) {
        self.start_recovery_observed(fan, now, cause, None);
    }
    fn start_recovery_observed(
        &mut self,
        fan: FanId,
        now: u64,
        cause: Option<Fault>,
        observed: Option<HardwareState>,
    ) {
        let lease = match self.entries.get(&fan) {
            Some(Entry::Active(lease)) => lease.clone(),
            _ => return,
        };
        self.entries.remove(&fan);
        // No lease secret or owner survives revocation.
        let fault = cause.unwrap_or(Fault::Released);
        let recovery = Recovery {
            cause: fault,
            origin: if lease.attempted_target.is_some() {
                RecoveryOrigin::TargetUpdate
            } else if lease.committed {
                RecoveryOrigin::CommittedLease
            } else {
                RecoveryOrigin::UnconfirmedTransaction
            },
            baseline_target: if lease.committed {
                Some(lease.target)
            } else {
                observed.and_then(Self::finite_manual_target)
            },
            attempted_target: lease.attempted_target,
            attempts: 0,
            next_retry: None,
            last_error: RecoveryError::ReadbackMismatch,
        };
        self.attempt_recovery(fan, recovery, now, cause);
    }
    fn finite_manual_target(state: HardwareState) -> Option<f64> {
        if state.mode == Mode::Manual {
            state.target_rpm.filter(|rpm| rpm.is_finite())
        } else {
            None
        }
    }
    fn attempt_recovery(
        &mut self,
        fan: FanId,
        mut recovery: Recovery,
        now: u64,
        confirmed_cause: Option<Fault>,
    ) {
        if matches!(recovery.origin, RecoveryOrigin::TargetUpdate) {
            // A failed write may have left either our old or attempted target.
            // Neither is competition. Any other finite manual target belongs to
            // another controller; unknown/read-error state never authorizes a
            // recovery write for this transaction.
            match self.actuator.read_state(fan) {
                Ok(state) if state.mode == Mode::Automatic => {
                    if recovery.attempts == 0 {
                        self.entries.insert(fan, Entry::Yielded(state));
                    } else {
                        self.entries.insert(fan, Entry::Automatic(confirmed_cause));
                    }
                    return;
                }
                Ok(state)
                    if state.mode == Mode::Manual
                        && state.target_rpm.is_some_and(|target| {
                            target.is_finite()
                                && (Some(target) == recovery.baseline_target
                                    || Some(target) == recovery.attempted_target)
                        }) => {}
                Ok(state) if Self::finite_manual_target(state).is_some() => {
                    self.entries.insert(fan, Entry::Yielded(state));
                    return;
                }
                observation => {
                    let failure = match observation {
                        Err(error) => RecoveryError::Read(error),
                        _ => RecoveryError::ReadbackMismatch,
                    };
                    recovery.attempts += 1;
                    self.record_recovery_failure(fan, recovery, now, failure);
                    return;
                }
            }
        }
        // Every committed lease checks for competition even when recovery was
        // triggered by a capability/read fault. Only the first recovery of an
        // unconfirmed transaction may force automatic without this guard.
        let guard = !matches!(recovery.origin, RecoveryOrigin::TargetUpdate)
            && (matches!(recovery.origin, RecoveryOrigin::CommittedLease) || recovery.attempts > 0);
        if guard {
            match self.actuator.read_state(fan) {
                Ok(s) if s.mode == Mode::Automatic => {
                    self.entries.insert(fan, Entry::Automatic(confirmed_cause));
                    return;
                }
                Ok(s) => {
                    if let Some(target) = Self::finite_manual_target(s) {
                        match recovery.baseline_target {
                            Some(baseline) if target != baseline => {
                                self.entries.insert(fan, Entry::Yielded(s));
                                return;
                            }
                            None => recovery.baseline_target = Some(target),
                            _ => {}
                        }
                    }
                }
                Err(_) => {}
            }
        }
        recovery.attempts += 1;
        let write = self.actuator.write_mode(fan, WriteMode::Automatic);
        // Always perform the read, including when write reports a partial error.
        let read = self.actuator.read_state(fan);
        // Keep the first failure-site observation stable across retries. A bad
        // target left by our unconfirmed transaction is not evidence that an
        // external controller subsequently changed it.
        if recovery.baseline_target.is_none() {
            recovery.baseline_target = read
                .as_ref()
                .ok()
                .and_then(|state| Self::finite_manual_target(*state));
        }
        let failure = match (write, read) {
            (Ok(()), Ok(s)) if s.mode == Mode::Automatic => {
                self.entries.insert(fan, Entry::Automatic(confirmed_cause));
                return;
            }
            (Err(e), _) => RecoveryError::Write(e),
            (_, Err(e)) => RecoveryError::Read(e),
            _ => RecoveryError::ReadbackMismatch,
        };
        self.record_recovery_failure(fan, recovery, now, failure);
    }
    fn record_recovery_failure(
        &mut self,
        fan: FanId,
        mut recovery: Recovery,
        now: u64,
        failure: RecoveryError,
    ) {
        recovery.last_error = failure;
        recovery.next_retry = if recovery.attempts < RECOVERY_ATTEMPTS {
            now.checked_add(RECOVERY_RETRY_MS)
        } else {
            None
        };
        self.entries.insert(fan, Entry::Recovery(recovery));
    }
    fn poll(&mut self, now: u64) {
        let fans: Vec<_> = self.entries.keys().copied().collect();
        for fan in fans {
            match self.entries.get(&fan) {
                Some(Entry::Active(_)) => {
                    let _ = self.check_active(fan, now);
                }
                Some(Entry::Recovery(r)) if r.next_retry.is_some_and(|at| now >= at) => {
                    if let Some(Entry::Recovery(r)) = self.entries.remove(&fan) {
                        let cause = Some(r.cause);
                        self.attempt_recovery(fan, r, now, cause);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod target_update_tests {
    use super::*;
    use std::collections::VecDeque;

    const FAN: FanId = FanId(0);
    fn owner(id: u8) -> Owner {
        Owner::from_authenticated_transport([id; 32])
    }
    fn manual(target: f64) -> HardwareState {
        HardwareState {
            mode: Mode::Manual,
            target_rpm: Some(target),
        }
    }
    #[derive(Clone, Copy)]
    enum Effect {
        Pass,
        Fail,
        PartialFail,
        Ignore,
    }
    struct Fake {
        caps: Capabilities,
        state: HardwareState,
        reads: VecDeque<Result<HardwareState, ActuatorError>>,
        targets: VecDeque<Effect>,
        modes: VecDeque<Effect>,
        writes: Vec<(Option<f64>, Option<WriteMode>)>,
    }
    impl Actuator for Fake {
        fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
            Ok(vec![FAN])
        }
        fn capabilities(&mut self, _: FanId) -> Result<Capabilities, ActuatorError> {
            Ok(self.caps.clone())
        }
        fn read_state(&mut self, _: FanId) -> Result<HardwareState, ActuatorError> {
            match self.reads.pop_front() {
                Some(Ok(state)) => {
                    self.state = state;
                    Ok(state)
                }
                Some(Err(error)) => Err(error),
                None => Ok(self.state),
            }
        }
        fn write_target(&mut self, _: FanId, rpm: f64) -> Result<(), ActuatorError> {
            self.writes.push((Some(rpm), None));
            let effect = self.targets.pop_front().unwrap_or(Effect::Pass);
            if matches!(effect, Effect::Pass | Effect::PartialFail) {
                self.state.target_rpm = Some(rpm);
            }
            if matches!(effect, Effect::Fail | Effect::PartialFail) {
                Err(ActuatorError::Io)
            } else {
                Ok(())
            }
        }
        fn write_mode(&mut self, _: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
            self.writes.push((None, Some(mode)));
            let effect = self.modes.pop_front().unwrap_or(Effect::Pass);
            if matches!(effect, Effect::Pass | Effect::PartialFail) {
                self.state.mode = match mode {
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
    struct Tokens;
    impl TokenSource for Tokens {
        fn next_token(&mut self) -> Result<[u8; 32], TokenError> {
            Ok([7; 32])
        }
    }
    type TestEngine = Engine<Fake, Tokens>;
    fn fixture(now: u64) -> (TestEngine, LeaseGrant) {
        let fake = Fake {
            caps: Capabilities {
                fan: FAN,
                validated: true,
                model: ModelSupport::Verified,
                encoding: TargetEncoding::Fpe2,
                mode_control: ModeControl::PerFan,
                min_rpm: 1000.0,
                max_rpm: 5000.0,
                step_rpm: 25.0,
                stages_target_while_automatic: true,
            },
            state: HardwareState {
                mode: Mode::Automatic,
                target_rpm: Some(1500.0),
            },
            reads: VecDeque::new(),
            targets: VecDeque::new(),
            modes: VecDeque::new(),
            writes: vec![],
        };
        let mut engine = Engine::new(fake, Tokens);
        let Response::Lease(grant) = engine
            .execute(
                owner(1),
                Command::Acquire {
                    fan: FAN,
                    rpm: 3000.0,
                    ttl_ms: 100,
                    seq: 1,
                },
                now,
            )
            .unwrap()
        else {
            panic!("missing grant")
        };
        engine.actuator.writes.clear();
        (engine, grant)
    }
    fn update(
        engine: &mut TestEngine,
        grant: &LeaseGrant,
        rpm: f64,
        seq: u64,
        now: u64,
    ) -> Result<Response, Error> {
        engine.execute(
            owner(1),
            Command::SetTarget {
                fan: FAN,
                token: grant.token.clone(),
                rpm,
                ttl_ms: 200,
                seq,
            },
            now,
        )
    }
    fn phase(engine: &TestEngine) -> Phase {
        engine.status(FAN).unwrap().phase
    }
    fn fault(result: Result<Response, Error>, expected: Fault) {
        assert!(matches!(result, Err(Error::Fault { cause, .. }) if cause == expected));
    }

    #[test]
    fn update_commits_verified_target_same_token_and_release_uses_new_baseline() {
        let (mut engine, grant) = fixture(0);
        let Response::Lease(next) = update(&mut engine, &grant, 3531.0, 2, 10).unwrap() else {
            panic!()
        };
        assert_eq!(
            next,
            LeaseGrant {
                target_rpm: 3525.0,
                expires_at_ms: 210,
                seq: 2,
                ..grant.clone()
            }
        );
        assert_eq!(engine.actuator.writes, vec![(Some(3525.0), None)]);
        assert_eq!(engine.issued_tokens.len(), 1);
        assert_eq!(
            engine.execute(
                owner(1),
                Command::Release {
                    fan: FAN,
                    token: next.token,
                    seq: 3
                },
                20
            ),
            Ok(Response::AutomaticConfirmed)
        );
    }

    #[test]
    fn update_quantized_alias_renews_after_readback_without_writing() {
        let (mut engine, grant) = fixture(0);
        let Response::Lease(next) = update(&mut engine, &grant, 3001.0, 2, 10).unwrap() else {
            panic!()
        };
        assert_eq!(next.target_rpm, 3000.0);
        assert_eq!(next.token, grant.token);
        assert_eq!(next.expires_at_ms, 210);
        assert_eq!(next.seq, 2);
        assert!(engine.actuator.writes.is_empty());
        engine.actuator.state = manual(4000.0);
        fault(
            update(&mut engine, &next, 3001.0, 3, 20),
            Fault::ExternalControl,
        );
        assert!(engine.actuator.writes.is_empty());
    }

    #[test]
    fn update_rejects_identity_sequence_range_and_ttl_without_writes_or_commit() {
        let (mut engine, grant) = fixture(0);
        let command = |token, rpm, ttl_ms, seq| Command::SetTarget {
            fan: FAN,
            token,
            rpm,
            ttl_ms,
            seq,
        };
        assert_eq!(
            engine.execute(owner(2), command(grant.token.clone(), 3500.0, 200, 2), 1),
            Err(Error::OwnerConflict)
        );
        assert_eq!(
            engine.execute(owner(1), command(LeaseToken([8; 32]), 3500.0, 200, 2), 2),
            Err(Error::InvalidToken)
        );
        assert_eq!(
            update(&mut engine, &grant, 3500.0, 1, 3),
            Err(Error::InvalidSequence)
        );
        for (index, rpm) in [f64::NAN, f64::INFINITY, 999.9, 5000.1]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                update(&mut engine, &grant, rpm, 2, 4 + index as u64),
                Err(Error::InvalidRpm)
            );
        }
        for (index, ttl) in [0, MAX_TTL_MS + 1].into_iter().enumerate() {
            assert_eq!(
                engine.execute(
                    owner(1),
                    command(grant.token.clone(), 3500.0, ttl, 2),
                    8 + index as u64
                ),
                Err(Error::InvalidTtl)
            );
        }
        assert_eq!(
            phase(&engine),
            Phase::Active {
                target_rpm: 3000.0,
                expires_at_ms: 100,
                seq: 1
            }
        );
        assert!(engine.actuator.writes.is_empty());
    }

    #[test]
    fn update_deadline_overflow_and_expiration_never_revive_or_write_target() {
        let (mut engine, grant) = fixture(u64::MAX - 150);
        assert_eq!(
            update(&mut engine, &grant, 3500.0, 2, u64::MAX - 149),
            Err(Error::DeadlineOverflow)
        );
        assert!(engine.actuator.writes.is_empty());
        let (mut engine, grant) = fixture(0);
        assert_eq!(
            update(&mut engine, &grant, 3500.0, 2, 100),
            Err(Error::NoActiveLease)
        );
        assert_eq!(
            engine.actuator.writes,
            vec![(None, Some(WriteMode::Automatic))]
        );
        engine.actuator.writes.clear();
        assert_eq!(
            update(&mut engine, &grant, 3500.0, 2, 101),
            Err(Error::NoActiveLease)
        );
        assert!(engine.actuator.writes.is_empty());
    }

    #[test]
    fn update_capability_change_and_external_control_do_not_write_target() {
        let (mut engine, grant) = fixture(0);
        engine.actuator.caps.max_rpm = 4500.0;
        fault(
            update(&mut engine, &grant, 3500.0, 2, 1),
            Fault::CapabilityChanged,
        );
        assert_eq!(
            engine.actuator.writes,
            vec![(None, Some(WriteMode::Automatic))]
        );
        for state in [
            manual(4000.0),
            HardwareState {
                mode: Mode::Automatic,
                target_rpm: Some(3000.0),
            },
        ] {
            let (mut engine, grant) = fixture(0);
            engine.actuator.state = state;
            fault(
                update(&mut engine, &grant, 3500.0, 2, 1),
                Fault::ExternalControl,
            );
            assert!(matches!(phase(&engine), Phase::Yielded { .. }));
            assert!(engine.actuator.writes.is_empty());
        }
    }

    #[test]
    fn update_write_errors_before_and_after_effect_recover_owned_targets() {
        for effect in [Effect::Fail, Effect::PartialFail] {
            let (mut engine, grant) = fixture(0);
            engine.actuator.targets.push_back(effect);
            fault(
                update(&mut engine, &grant, 3500.0, 2, 1),
                Fault::TargetWrite,
            );
            assert_eq!(
                phase(&engine),
                Phase::AutomaticConfirmed {
                    last_fault: Some(Fault::TargetWrite)
                }
            );
            assert_eq!(
                engine.actuator.writes,
                vec![(Some(3500.0), None), (None, Some(WriteMode::Automatic))]
            );
            assert_eq!(
                update(&mut engine, &grant, 3500.0, 2, 2),
                Err(Error::NoActiveLease)
            );
        }
    }

    #[test]
    fn update_ignored_write_or_read_error_cannot_commit_and_recovers_owned_state() {
        for read_error in [false, true] {
            let (mut engine, grant) = fixture(0);
            if read_error {
                engine
                    .actuator
                    .reads
                    .extend([Ok(manual(3000.0)), Err(ActuatorError::Timeout)]);
            } else {
                engine.actuator.targets.push_back(Effect::Ignore);
            }
            fault(
                update(&mut engine, &grant, 3500.0, 2, 1),
                Fault::TargetReadback,
            );
            assert_eq!(
                phase(&engine),
                Phase::AutomaticConfirmed {
                    last_fault: Some(Fault::TargetReadback)
                }
            );
            assert_eq!(engine.actuator.writes.len(), 2);
        }
    }

    #[test]
    fn update_readback_external_target_or_mode_yields_without_restore_write() {
        for state in [
            manual(4100.0),
            HardwareState {
                mode: Mode::Automatic,
                target_rpm: Some(3500.0),
            },
        ] {
            let (mut engine, grant) = fixture(0);
            engine
                .actuator
                .reads
                .extend([Ok(manual(3000.0)), Ok(state)]);
            fault(
                update(&mut engine, &grant, 3500.0, 2, 1),
                Fault::ExternalControl,
            );
            assert!(matches!(phase(&engine), Phase::Yielded { observed, .. } if observed == state));
            assert_eq!(engine.actuator.writes, vec![(Some(3500.0), None)]);
        }
    }

    #[test]
    fn update_failed_restore_retries_old_or_attempted_target_with_bounded_budget() {
        for effect in [Effect::Fail, Effect::PartialFail] {
            let (mut engine, grant) = fixture(0);
            engine.actuator.targets.push_back(effect);
            engine.actuator.modes.extend([Effect::Fail; 3]);
            fault(
                update(&mut engine, &grant, 3500.0, 2, 1),
                Fault::TargetWrite,
            );
            for now in [250, 251, 501, 751, 1001] {
                engine
                    .execute(owner(1), Command::Tick(TickReason::Periodic), now)
                    .unwrap();
            }
            assert!(matches!(
                phase(&engine),
                Phase::Recovering {
                    attempts: RECOVERY_ATTEMPTS,
                    exhausted: true,
                    ..
                }
            ));
            assert_eq!(engine.actuator.writes.len(), 4);
            assert_eq!(
                update(&mut engine, &grant, 3500.0, 2, 1002),
                Err(Error::RecoveryPending)
            );
        }
    }

    #[test]
    fn update_recovery_yields_to_later_external_target_and_unknown_reads_never_write() {
        let (mut engine, grant) = fixture(0);
        engine.actuator.targets.push_back(Effect::PartialFail);
        engine.actuator.modes.push_back(Effect::Fail);
        fault(
            update(&mut engine, &grant, 3500.0, 2, 1),
            Fault::TargetWrite,
        );
        engine.actuator.state = manual(4000.0);
        engine
            .execute(owner(1), Command::Tick(TickReason::Periodic), 251)
            .unwrap();
        assert!(matches!(phase(&engine), Phase::Yielded { .. }));
        assert_eq!(engine.actuator.writes.len(), 2);

        let (mut engine, grant) = fixture(0);
        engine.actuator.targets.push_back(Effect::PartialFail);
        engine.actuator.reads.extend([
            Ok(manual(3000.0)),
            Err(ActuatorError::Timeout),
            Err(ActuatorError::Timeout),
            Err(ActuatorError::Timeout),
        ]);
        fault(
            update(&mut engine, &grant, 3500.0, 2, 1),
            Fault::TargetWrite,
        );
        engine
            .execute(owner(1), Command::Tick(TickReason::Periodic), 251)
            .unwrap();
        engine
            .execute(owner(1), Command::Tick(TickReason::Periodic), 501)
            .unwrap();
        assert!(matches!(
            phase(&engine),
            Phase::Recovering {
                attempts: 3,
                exhausted: true,
                ..
            }
        ));
        assert_eq!(engine.actuator.writes, vec![(Some(3500.0), None)]);
    }
}
