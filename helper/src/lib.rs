//! Strict JSON adaptation and C ABI for a macOS NSXPC dry-run host.
//! Production service remains read-only; the offline control constructor is profile gated.
//! Synchronous IOKit reads cannot be preempted by the host timer.

use macs_fan_control_core::*;
use macs_fan_control_runtime::{Clock, Coordinator, StopReason};
mod plans;
mod sensors;
use macs_fan_smc_adapter::{AdapterError, ControlAdmission, SmcAdapter};
use plans::{PlanAuthorization, PlanRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::{c_char, c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const MAX_REQUEST_BYTES: usize = 4096;
const VERSION: u8 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    command: Request,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    StartPlan {
        entries: Vec<plans::WireEntry>,
        ttl_ms: u64,
        seq: u64,
    },
    RenewPlan {
        token: String,
        ttl_ms: u64,
        seq: u64,
    },
    StopPlan {
        token: String,
        seq: u64,
    },
    PlanStatus {},
    Enumerate {},
    InspectHardware {},
    Capabilities {
        fan: u16,
    },
    Acquire {
        fan: u16,
        rpm: f64,
        ttl_ms: u64,
        seq: u64,
    },
    Renew {
        fan: u16,
        token: String,
        ttl_ms: u64,
        seq: u64,
    },
    SetTarget {
        fan: u16,
        token: String,
        rpm: f64,
        ttl_ms: u64,
        seq: u64,
    },
    Release {
        fan: u16,
        token: String,
        seq: u64,
    },
}

fn token(text: &str) -> Result<LeaseToken, &'static str> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("invalid_token_encoding");
    }
    let mut bytes = [0; 32];
    for (out, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
        *out = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(LeaseToken::from_transport_bytes(bytes))
}
fn token_text(token: &LeaseToken) -> String {
    token
        .transport_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
enum WireCommand {
    Core(Command),
    InspectHardware,
    Plan(PlanRequest),
}
fn parse(bytes: &[u8]) -> Result<WireCommand, &'static str> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large");
    }
    let request: Envelope = serde_json::from_slice(bytes).map_err(|_| "invalid_schema")?;
    if request.version != VERSION {
        return Err("unsupported_version");
    }
    Ok(WireCommand::Core(match request.command {
        Request::StartPlan {
            entries,
            ttl_ms,
            seq,
        } => {
            return Ok(WireCommand::Plan(PlanRequest::Start {
                entries,
                ttl_ms,
                seq,
            }))
        }
        Request::RenewPlan {
            token: secret,
            ttl_ms,
            seq,
        } => {
            return Ok(WireCommand::Plan(PlanRequest::Renew {
                token: token(&secret)?,
                ttl_ms,
                seq,
            }))
        }
        Request::StopPlan { token: secret, seq } => {
            return Ok(WireCommand::Plan(PlanRequest::Stop {
                token: token(&secret)?,
                seq,
            }))
        }
        Request::PlanStatus {} => return Ok(WireCommand::Plan(PlanRequest::Status)),
        Request::InspectHardware {} => return Ok(WireCommand::InspectHardware),
        Request::Enumerate {} => Command::Enumerate,
        Request::Capabilities { fan } => Command::Capabilities { fan: FanId(fan) },
        Request::Acquire {
            fan,
            rpm,
            ttl_ms,
            seq,
        } => Command::Acquire {
            fan: FanId(fan),
            rpm,
            ttl_ms,
            seq,
        },
        Request::Renew {
            fan,
            token: secret,
            ttl_ms,
            seq,
        } => Command::Renew {
            fan: FanId(fan),
            token: token(&secret)?,
            ttl_ms,
            seq,
        },
        Request::SetTarget {
            fan,
            token: secret,
            rpm,
            ttl_ms,
            seq,
        } => Command::SetTarget {
            fan: FanId(fan),
            token: token(&secret)?,
            rpm,
            ttl_ms,
            seq,
        },
        Request::Release {
            fan,
            token: secret,
            seq,
        } => Command::Release {
            fan: FanId(fan),
            token: token(&secret)?,
            seq,
        },
    }))
}

struct OsTokens;
#[cfg(test)]
thread_local! {
    static TOKEN_REQUESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecRandomCopyBytes(random: *const c_void, count: usize, bytes: *mut c_void) -> i32;
}
impl TokenSource for OsTokens {
    fn next_token(&mut self) -> Result<[u8; 32], TokenError> {
        #[cfg(test)]
        TOKEN_REQUESTS.with(|count| count.set(count.get() + 1));
        #[cfg(target_os = "macos")]
        {
            let mut bytes = [0; 32];
            // SAFETY: Security's default RNG accepts NULL; this writable buffer
            // is exactly the supplied length and remains alive during the call.
            let status = unsafe {
                SecRandomCopyBytes(std::ptr::null(), bytes.len(), bytes.as_mut_ptr().cast())
            };
            if status == 0 {
                Ok(bytes)
            } else {
                Err(TokenError)
            }
        }
        #[cfg(not(target_os = "macos"))]
        Err(TokenError)
    }
}

enum Backend<A = SmcAdapter> {
    Fake {
        state: HardwareState,
        #[cfg(any(test, feature = "test-harness"))]
        automatic_failures: u8,
    },
    Unavailable,
    SmcReadonly(Arc<Mutex<A>>),
    SmcControl(Arc<Mutex<A>>),
}
impl<A: Actuator> Actuator for Backend<A> {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        match self {
            Self::Fake { .. } => Ok(vec![FanId(0)]),
            Self::SmcReadonly(adapter) | Self::SmcControl(adapter) => adapter
                .lock()
                .map_err(|_| ActuatorError::Unavailable)?
                .enumerate(),
            Self::Unavailable => Err(ActuatorError::Unavailable),
        }
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        if let Self::SmcReadonly(adapter) | Self::SmcControl(adapter) = self {
            return adapter
                .lock()
                .map_err(|_| ActuatorError::Unavailable)?
                .capabilities(fan);
        }
        if fan != FanId(0) {
            return Err(ActuatorError::Unsupported);
        }
        match self {
            Self::Fake { .. } => Ok(Capabilities {
                fan,
                validated: true,
                model: ModelSupport::Verified,
                encoding: TargetEncoding::Fpe2,
                mode_control: ModeControl::PerFan,
                min_rpm: 1000.0,
                max_rpm: 5000.0,
                step_rpm: 0.25,
                stages_target_while_automatic: true,
            }),
            Self::Unavailable => Err(ActuatorError::Unavailable),
            Self::SmcReadonly(_) | Self::SmcControl(_) => Err(ActuatorError::Unsupported),
        }
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        if let Self::SmcReadonly(adapter) | Self::SmcControl(adapter) = self {
            return adapter
                .lock()
                .map_err(|_| ActuatorError::Unavailable)?
                .read_state(fan);
        }
        if fan != FanId(0) {
            return Err(ActuatorError::Unsupported);
        }
        match self {
            Self::Fake { state, .. } => Ok(*state),
            Self::Unavailable => Err(ActuatorError::Unavailable),
            Self::SmcReadonly(_) | Self::SmcControl(_) => Err(ActuatorError::Unsupported),
        }
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        if let Self::SmcControl(adapter) = self {
            return adapter
                .lock()
                .map_err(|_| ActuatorError::Unavailable)?
                .write_target(fan, rpm);
        }
        if fan != FanId(0) {
            return Err(ActuatorError::Unsupported);
        }
        match self {
            Self::Fake { state, .. } => {
                state.target_rpm = Some(rpm);
                Ok(())
            }
            Self::Unavailable => Err(ActuatorError::Unavailable),
            Self::SmcReadonly(_) | Self::SmcControl(_) => Err(ActuatorError::Unsupported),
        }
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        if let Self::SmcControl(adapter) = self {
            return adapter
                .lock()
                .map_err(|_| ActuatorError::Unavailable)?
                .write_mode(fan, mode);
        }
        if fan != FanId(0) {
            return Err(ActuatorError::Unsupported);
        }
        match self {
            Self::Fake {
                state,
                #[cfg(any(test, feature = "test-harness"))]
                automatic_failures,
            } => {
                #[cfg(any(test, feature = "test-harness"))]
                if mode == WriteMode::Automatic && *automatic_failures > 0 {
                    *automatic_failures -= 1;
                    return Err(ActuatorError::Io);
                }
                state.mode = if mode == WriteMode::Automatic {
                    Mode::Automatic
                } else {
                    Mode::Manual
                };
                Ok(())
            }
            Self::Unavailable => Err(ActuatorError::Unavailable),
            Self::SmcReadonly(_) | Self::SmcControl(_) => Err(ActuatorError::Unsupported),
        }
    }
}

// This fence is checked immediately before each target/manual operation, including
// after slow preflight or a prior blocking write. It cannot cancel I/O already entered.
struct GuardedBackend<A = SmcAdapter> {
    backend: Backend<A>,
    origin: Instant,
    plan_deadline: Arc<AtomicU64>,
}
impl<A> GuardedBackend<A> {
    fn permit_write(&self) -> Result<(), ActuatorError> {
        let now = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        if now >= self.plan_deadline.load(Ordering::SeqCst) {
            Err(ActuatorError::Timeout)
        } else {
            Ok(())
        }
    }
}
impl<A: Actuator> Actuator for GuardedBackend<A> {
    fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
        self.backend.enumerate()
    }
    fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
        self.backend.capabilities(fan)
    }
    fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
        self.backend.read_state(fan)
    }
    fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
        self.permit_write()?;
        self.backend.write_target(fan, rpm)
    }
    fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
        if mode == WriteMode::Manual {
            self.permit_write()?;
        }
        self.backend.write_mode(fan, mode)
    }
}

fn status_json(status: &FanStatus) -> Value {
    let phase = match &status.phase {
        Phase::Active {
            target_rpm,
            expires_at_ms,
            seq,
        } => {
            json!({"state":"active", "target_rpm":target_rpm, "expires_at_ms":expires_at_ms, "seq":seq})
        }
        Phase::AutomaticConfirmed { last_fault } => {
            json!({"state":"automatic_confirmed", "cause":last_fault.map(|cause| format!("{cause:?}"))})
        }
        Phase::Recovering {
            cause,
            attempts,
            next_retry_at_ms,
            exhausted,
            last_error,
        } => {
            json!({"state":"recovering", "cause":format!("{cause:?}"), "attempts":attempts, "next_retry_at_ms":next_retry_at_ms, "exhausted":exhausted, "last_error":format!("{last_error:?}")})
        }
        Phase::Yielded { cause, observed } => {
            json!({"state":"yielded", "cause":format!("{cause:?}"), "observed_mode":format!("{:?}", observed.mode), "observed_target_rpm":observed.target_rpm})
        }
    };
    json!({"fan":status.fan.0, "phase":phase})
}
fn error_code(error: &Error) -> &'static str {
    match error {
        Error::UnknownFan => "unknown_fan",
        Error::InvalidInventory => "invalid_inventory",
        Error::UnverifiedCapabilities => "unverified_capabilities",
        Error::InvalidRpm => "invalid_rpm",
        Error::InvalidTtl => "invalid_ttl",
        Error::InvalidSequence => "invalid_sequence",
        Error::DeadlineOverflow => "deadline_overflow",
        Error::OwnerConflict => "owner_conflict",
        Error::InvalidToken => "invalid_token",
        Error::NoActiveLease => "no_active_lease",
        Error::RecoveryPending => "recovery_pending",
        Error::EntropyUnavailable => "entropy_unavailable",
        Error::ClockRollback => "clock_rollback",
        Error::ClockUntrusted => "clock_untrusted",
        Error::Suspended => "suspended",
        Error::ExternalController => "external_controller",
        Error::Actuator(_) => "actuator_unavailable",
        Error::Fault { .. } => "control_fault",
    }
}
fn response_json(response: Response) -> Value {
    match response {
        Response::Fans(fans) => {
            json!({"kind":"fans", "fans":fans.into_iter().map(|f|f.0).collect::<Vec<_>>()})
        }
        Response::Capabilities(c) => {
            json!({"kind":"capabilities", "fan":c.fan.0, "validated":c.validated,
            "min_rpm":c.min_rpm,"max_rpm":c.max_rpm,"step_rpm":c.step_rpm,"encoding":format!("{:?}",c.encoding),
            "model":format!("{:?}",c.model),"mode_control":format!("{:?}",c.mode_control),
            "stages_target_while_automatic":c.stages_target_while_automatic})
        }
        Response::Lease(grant) => {
            json!({"kind":"lease", "fan":grant.fan.0, "token":token_text(&grant.token),
            "target_rpm":grant.target_rpm, "expires_at_ms":grant.expires_at_ms,"seq":grant.seq})
        }
        Response::AutomaticConfirmed => json!({"kind":"automatic_confirmed"}),
        Response::Statuses(statuses) => {
            json!({"kind":"statuses", "fans":statuses.iter().map(status_json).collect::<Vec<_>>()})
        }
    }
}

struct HostClock(Instant);
impl Clock for HostClock {
    fn now_ms(&mut self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
struct Host {
    state: Mutex<HostState>,
}
// Constructor-selected and never changed by requests, probes, or lifecycle events.
// One closed value prevents backend/flags/admission from drifting independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostExecution {
    ReadonlyPolicy,
    NoReviewedProfile,
    ProfileValidationFailed,
    BackendOpenFailed,
    BackendUnavailable,
    Admitted,
    TestOnly,
}
impl HostExecution {
    fn backend(self) -> &'static str {
        match self {
            Self::ReadonlyPolicy | Self::NoReviewedProfile | Self::ProfileValidationFailed => {
                "smc_readonly"
            }
            Self::BackendOpenFailed | Self::BackendUnavailable => "unavailable",
            Self::Admitted => "smc_control",
            Self::TestOnly => "fake",
        }
    }
    fn admission(self) -> &'static str {
        match self {
            Self::ReadonlyPolicy => "readonly_policy",
            Self::NoReviewedProfile => "no_reviewed_profile",
            Self::ProfileValidationFailed => "profile_validation_failed",
            Self::BackendOpenFailed => "backend_open_failed",
            Self::BackendUnavailable => "backend_unavailable",
            Self::Admitted => "admitted",
            Self::TestOnly => "test_only",
        }
    }
    fn execution(self) -> &'static str {
        match self {
            Self::Admitted => "profile_gated",
            Self::TestOnly => "test_fake",
            _ => "readonly",
        }
    }
    fn permits_plan(self) -> bool {
        matches!(self, Self::Admitted | Self::TestOnly)
    }
}

struct HostState {
    engine: Coordinator<GuardedBackend, OsTokens, HostClock>,
    plan_deadline: Arc<AtomicU64>,
    sampler: sensors::Sampler,
    sensor_batch: Option<sensors::Batch>,
    plan_sensor_ids: std::collections::BTreeSet<String>,
    plan_sample_sequence: u64,
    plan_authorization: Option<PlanAuthorization>,
    authorization_end: Option<&'static str>,
    issued_plan_tokens: std::collections::BTreeSet<[u8; 32]>,
    origin: Instant,
    execution: HostExecution,
    hardware_reads: bool,
    smc: Option<Arc<Mutex<SmcAdapter>>>,
    hardware_write_attempts: Option<u64>,
    hardware_inspection_observed_at_ms: Option<u64>,
    backend_open_error: Option<AdapterError>,
    watchdog_ticks: u64,
    plan_watchdog_expirations: u64,
    watchdog_expirations: u64,
    lifecycle: &'static str,
}
impl HostState {
    fn new(fake: bool) -> Self {
        let backend = if fake {
            Backend::Fake {
                state: HardwareState {
                    mode: Mode::Automatic,
                    target_rpm: Some(1500.0),
                },
                #[cfg(any(test, feature = "test-harness"))]
                automatic_failures: 0,
            }
        } else {
            Backend::Unavailable
        };
        let origin = Instant::now();
        let plan_deadline = Arc::new(AtomicU64::new(u64::MAX));
        Self {
            engine: Coordinator::new(
                GuardedBackend {
                    backend,
                    origin,
                    plan_deadline: plan_deadline.clone(),
                },
                OsTokens,
                HostClock(origin),
            ),
            plan_deadline,
            sampler: if fake {
                sensors::Sampler::fake(origin)
            } else {
                sensors::Sampler::unavailable()
            },
            sensor_batch: None,
            plan_sensor_ids: std::collections::BTreeSet::new(),
            plan_sample_sequence: 0,
            plan_authorization: None,
            authorization_end: None,
            issued_plan_tokens: std::collections::BTreeSet::new(),
            origin,
            execution: if fake {
                HostExecution::TestOnly
            } else {
                HostExecution::BackendUnavailable
            },
            hardware_reads: false,
            smc: None,
            hardware_write_attempts: None,
            hardware_inspection_observed_at_ms: None,
            backend_open_error: None,
            watchdog_ticks: 0,
            plan_watchdog_expirations: 0,
            watchdog_expirations: 0,
            lifecycle: "running",
        }
    }
    fn smc_readonly() -> Self {
        Self::from_smc_result(SmcAdapter::open_readonly())
    }
    fn smc_control() -> Self {
        Self::from_smc_control_result(SmcAdapter::open())
    }
    fn from_smc_result(opened: Result<SmcAdapter, AdapterError>) -> Self {
        match opened {
            Ok(adapter) => Self::with_smc(adapter, HostExecution::ReadonlyPolicy),
            Err(error) => Self::smc_open_failed(error),
        }
    }
    fn from_smc_control_result(opened: Result<SmcAdapter, AdapterError>) -> Self {
        match opened {
            Ok(mut adapter) => {
                let execution = match adapter.control_admission() {
                    ControlAdmission::NoReviewedProfile => HostExecution::NoReviewedProfile,
                    ControlAdmission::ProfileValidationFailed => {
                        HostExecution::ProfileValidationFailed
                    }
                    ControlAdmission::Admitted => HostExecution::Admitted,
                };
                Self::with_smc(adapter, execution)
            }
            Err(error) => Self::smc_open_failed(error),
        }
    }
    fn smc_open_failed(error: AdapterError) -> Self {
        let mut host = Self::new(false);
        host.execution = HostExecution::BackendOpenFailed;
        host.backend_open_error = Some(error);
        host
    }
    fn with_smc(adapter: SmcAdapter, execution: HostExecution) -> Self {
        let mut host = Self::new(false);
        let adapter = if execution == HostExecution::Admitted {
            adapter
        } else {
            adapter.into_readonly()
        };
        let adapter = Arc::new(Mutex::new(adapter));
        host.execution = execution;
        host.hardware_reads = true;
        // Independent worker owns all temperature initialization/I/O.
        // Raw candidates remain unadmitted and never enable control.
        host.sampler = sensors::Sampler::candidates(host.origin);
        host.smc = Some(adapter.clone());
        host.engine = Coordinator::new(
            GuardedBackend {
                backend: if execution == HostExecution::Admitted {
                    Backend::SmcControl(adapter)
                } else {
                    // A denied construction cannot forward writes even if a
                    // later read would pass validation. Recreate to re-admit.
                    Backend::SmcReadonly(adapter)
                },
                origin: host.origin,
                plan_deadline: host.plan_deadline.clone(),
            },
            OsTokens,
            HostClock(host.origin),
        );
        host
    }
    fn attach_execution(&self, reply: &mut Value, plan: bool) {
        reply["backend"] = json!(self.execution.backend());
        reply["dry_run"] = json!(self.execution != HostExecution::Admitted);
        reply["hardware_write_enabled"] = json!(self.execution == HostExecution::Admitted);
        if plan {
            reply["control"] = json!({"version":1,
                "execution":self.execution.execution(), "admission":self.execution.admission()});
        }
    }
    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn wrap(&self, result: Result<Response, Error>) -> Value {
        let mut reply = json!({"version":VERSION,
            "helper_process_id":std::process::id(),
            "watchdog_ticks":self.watchdog_ticks, "watchdog_expirations":self.watchdog_expirations,
            "lifecycle":self.lifecycle,
            "hardware_reads":self.hardware_reads,
            "hardware_write_attempts":self.hardware_write_attempts,
            "hardware_write_attempts_scope":"latest_hardware_inspection",
            "hardware_inspection_observed_at_ms":self.hardware_inspection_observed_at_ms,
            "backend_open_error":self.backend_open_error,
            "statuses":self.engine.statuses().iter().map(status_json).collect::<Vec<_>>()});
        self.attach_execution(&mut reply, false);
        match result {
            Ok(response) => {
                reply["ok"] = json!(true);
                reply["result"] = response_json(response);
            }
            Err(error) => {
                reply["ok"] = json!(false);
                reply["error"] = json!({"code":error_code(&error)});
            }
        }
        reply
    }
    fn execute(&mut self, owner: Owner, command: Command) -> Value {
        let result = match command {
            Command::Tick(TickReason::Periodic) => self
                .engine
                .watchdog()
                .map(|_| Response::Statuses(self.engine.statuses())),
            Command::Tick(TickReason::Disconnected(disconnected)) => {
                if self
                    .plan_authorization
                    .as_ref()
                    .is_some_and(|a| a.owner == disconnected)
                {
                    self.plan_authorization = None;
                    self.authorization_end = Some("disconnected");
                }
                self.engine
                    .disconnect(disconnected)
                    .map(|_| Response::Statuses(self.engine.statuses()))
            }
            Command::Tick(TickReason::Sleep | TickReason::Shutdown) => {
                let reason = if matches!(command, Command::Tick(TickReason::Sleep)) {
                    StopReason::Sleep
                } else {
                    StopReason::Shutdown
                };
                self.plan_authorization = None;
                self.sampler.stop();
                self.sensor_batch = None;
                self.authorization_end = Some(if reason == StopReason::Sleep {
                    "sleep"
                } else {
                    "shutdown"
                });
                self.engine
                    .stop(reason)
                    .map(|_| Response::Statuses(self.engine.statuses()))
            }
            Command::Tick(TickReason::ControlFault(_)) => {
                Err(macs_fan_control_runtime::Error::InvalidDirectCommand)
            }
            command => self.engine.execute_direct(owner, command),
        };
        match result {
            Ok(response) => self.wrap(Ok(response)),
            Err(macs_fan_control_runtime::Error::Core(error)) => self.wrap(Err(error)),
            Err(macs_fan_control_runtime::Error::SessionLost(_)) => {
                self.wrap(Err(Error::NoActiveLease))
            }
            Err(error) => self.plan_error(plans::runtime_error(&error)),
        }
    }
    fn request(&mut self, owner: Owner, bytes: &[u8]) -> Value {
        self.maintain_plan(false);
        let parsed = parse(bytes);
        let requires_authorization = matches!(
            &parsed,
            Ok(WireCommand::Plan(
                PlanRequest::Start { .. } | PlanRequest::Renew { .. }
            ))
        );
        let reply = match parsed {
            Ok(WireCommand::Plan(command)) => self.plan_request(owner, command),
            Ok(WireCommand::Core(command)) => {
                if self.plan_authorization.is_none() {
                    self.plan_deadline.store(u64::MAX, Ordering::SeqCst);
                }
                self.execute(owner, command)
            }
            Ok(WireCommand::InspectHardware) => {
                // Preserve normal watchdog processing before and after potentially
                // blocking I/O. This cannot enforce a hard IOKit deadline.
                self.execute(owner, Command::Tick(TickReason::Periodic));
                let inspection = self.smc.as_ref().map(|adapter| {
                    adapter
                        .lock()
                        .map(|mut adapter| adapter.probe())
                        .map_err(|_| ())
                });
                let mut reply = self.execute(owner, Command::Tick(TickReason::Periodic));
                reply.as_object_mut().unwrap().remove("result");
                match inspection {
                    Some(Ok(report)) => {
                        self.hardware_write_attempts = Some(report.hardware_write_attempts);
                        self.hardware_inspection_observed_at_ms = Some(self.now());
                        reply["hardware_inspection_observed_at_ms"] =
                            json!(self.hardware_inspection_observed_at_ms);
                        reply["hardware_write_attempts"] = json!(report.hardware_write_attempts);
                        reply["ok"] = json!(true);
                        reply["result"] =
                            json!({"kind":"hardware_inspection", "inspection":report});
                        reply.as_object_mut().unwrap().remove("error");
                    }
                    _ => {
                        reply["ok"] = json!(false);
                        let code = if self.smc.is_some() || self.backend_open_error.is_some() {
                            "actuator_unavailable"
                        } else {
                            "not_supported"
                        };
                        reply["error"] = json!({"code":code});
                    }
                }
                reply
            }
            Err(code) => {
                let mut reply = self.execute(owner, Command::Tick(TickReason::Periodic));
                reply["ok"] = json!(false);
                reply["error"] = json!({"code":code});
                reply.as_object_mut().unwrap().remove("result");
                reply
            }
        };
        self.maintain_plan(false);
        self.refresh_reply(reply, requires_authorization)
    }
    fn watchdog_tick(&mut self) -> Value {
        let active: Vec<_> = self
            .engine
            .statuses()
            .into_iter()
            .filter(|status| matches!(status.phase, Phase::Active { .. }))
            .map(|status| status.fan)
            .collect();
        self.watchdog_ticks = self.watchdog_ticks.saturating_add(1);
        let had_authorization = self.plan_authorization.is_some();
        self.maintain_plan(true);
        if had_authorization
            && self.plan_authorization.is_none()
            && self.authorization_end == Some("expired")
        {
            self.plan_watchdog_expirations = self.plan_watchdog_expirations.saturating_add(1);
        }
        self.execute(
            Owner::from_authenticated_transport([0; 32]),
            Command::Tick(TickReason::Periodic),
        );
        for fan in active {
            if self.engine.status(fan).is_some_and(|status| {
                matches!(
                    status.phase,
                    Phase::AutomaticConfirmed {
                        last_fault: Some(Fault::Expired)
                    } | Phase::Recovering {
                        cause: Fault::Expired,
                        ..
                    }
                )
            }) {
                self.watchdog_expirations = self.watchdog_expirations.saturating_add(1);
            }
        }
        self.wrap(Ok(Response::Statuses(self.engine.statuses())))
    }
    fn lifecycle_begin(&mut self, event: u8) -> Value {
        let reason = match event {
            1 => {
                self.lifecycle = "sleep_locked";
                TickReason::Sleep
            }
            // Even if an earlier sleep notification was missed, wake revokes
            // and locks rather than reviving a pre-sleep lease or target.
            2 => {
                self.lifecycle = "wake_requires_new_session";
                TickReason::Sleep
            }
            3 => {
                self.lifecycle = "shutting_down";
                TickReason::Shutdown
            }
            _ => return protocol_error("invalid_host_event"),
        };
        self.execute(
            Owner::from_authenticated_transport([0; 32]),
            Command::Tick(reason),
        );
        self.lifecycle_report()
    }
    fn lifecycle_report(&self) -> Value {
        let statuses = self.engine.statuses();
        let confirmed = statuses
            .iter()
            .all(|s| matches!(s.phase, Phase::AutomaticConfirmed { .. }));
        let pending = statuses.iter().any(|s| {
            matches!(
                s.phase,
                Phase::Active { .. }
                    | Phase::Recovering {
                        exhausted: false,
                        ..
                    }
            )
        });
        let mut response = self.wrap(Ok(Response::Statuses(statuses)));
        response["recovery_confirmed"] = json!(confirmed);
        response["recovery_pending"] = json!(pending);
        response["recovery_scope"] = json!("tracked_leases_only");
        response
    }
    fn lifecycle_poll(&mut self) -> Value {
        self.watchdog_tick();
        self.lifecycle_report()
    }
}

fn encoded(value: Value) -> *mut c_char {
    CString::new(value.to_string())
        .expect("JSON escapes NUL")
        .into_raw()
}
fn protocol_error(code: &str) -> Value {
    json!({"version":VERSION,"dry_run":true,"hardware_write_enabled":false,
        "ok":false,"error":{"code":code}})
}
fn ffi_response(action: impl FnOnce() -> Value) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(value) => encoded(value),
        Err(_) => encoded(protocol_error("internal_failure")),
    }
}
unsafe fn with_host(handle: *mut c_void, action: impl FnOnce(&mut HostState) -> Value) -> Value {
    if handle.is_null() {
        return protocol_error("invalid_handle");
    }
    // SAFETY: Only mf_helper_create returns handles. The native host retains
    // them for process lifetime; this shared reference is protected by Mutex.
    let host = unsafe { &*handle.cast::<Host>() };
    match host.state.lock() {
        Ok(mut state) => action(&mut state),
        Err(_) => protocol_error("internal_failure"),
    }
}
unsafe fn owner_from_native(owner: *const u8) -> Option<Owner> {
    if owner.is_null() {
        return None;
    }
    // SAFETY: The ABI requires a live readable 32-byte session owner buffer.
    Some(Owner::from_authenticated_transport(unsafe {
        std::ptr::read_unaligned(owner.cast::<[u8; 32]>())
    }))
}

#[no_mangle]
pub extern "C" fn mf_helper_create(fake: u8) -> *mut c_void {
    if fake > 1 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Host {
        state: Mutex::new(HostState::new(fake == 1)),
    }))
    .cast()
}
/// Creates an unprivileged, irreversibly read-only SMC backend. Opening errors
/// remain structured in responses; this never falls back to fake hardware.
#[no_mangle]
pub extern "C" fn mf_helper_create_smc_readonly() -> *mut c_void {
    match catch_unwind(AssertUnwindSafe(|| {
        Box::into_raw(Box::new(Host {
            state: Mutex::new(HostState::smc_readonly()),
        }))
        .cast()
    })) {
        Ok(handle) => handle,
        Err(_) => std::ptr::null_mut(),
    }
}
/// Opens the real adapter and admits control only through its internal compiled
/// profile and full inventory validation. Denied hosts permanently use a backend
/// that cannot forward writes. The production service still uses readonly.
#[no_mangle]
pub extern "C" fn mf_helper_create_smc_control() -> *mut c_void {
    match catch_unwind(AssertUnwindSafe(|| {
        Box::into_raw(Box::new(Host {
            state: Mutex::new(HostState::smc_control()),
        }))
        .cast()
    })) {
        Ok(handle) => handle,
        Err(_) => std::ptr::null_mut(),
    }
}
/// # Safety
/// Handle must be created by this library. Owner and JSON buffers must remain
/// valid for their specified sizes for the call. Free returned strings once.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_request(
    handle: *mut c_void,
    owner: *const u8,
    bytes: *const u8,
    size: usize,
) -> *mut c_char {
    ffi_response(|| {
        let Some(owner) = (unsafe { owner_from_native(owner) }) else {
            return protocol_error("invalid_session");
        };
        if size > MAX_REQUEST_BYTES {
            return protocol_error("request_too_large");
        }
        if bytes.is_null() {
            return protocol_error("invalid_schema");
        }
        // SAFETY: The native ABI caller guarantees the non-null input's length.
        let bytes = unsafe { std::slice::from_raw_parts(bytes, size) };
        unsafe { with_host(handle, |host| host.request(owner, bytes)) }
    })
}
/// # Safety
/// Same handle/owner lifetime contract as mf_helper_request.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_disconnect(
    handle: *mut c_void,
    owner: *const u8,
) -> *mut c_char {
    ffi_response(|| {
        let Some(owner) = (unsafe { owner_from_native(owner) }) else {
            return protocol_error("invalid_session");
        };
        unsafe {
            with_host(handle, |host| {
                host.execute(owner, Command::Tick(TickReason::Disconnected(owner)))
            })
        }
    })
}
/// # Safety
/// The handle must be created by this library and remain live for this call.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_tick(handle: *mut c_void) -> *mut c_char {
    ffi_response(|| unsafe { with_host(handle, HostState::watchdog_tick) })
}
/// # Safety
/// The handle must be created by this library and remain live for this call.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_shutdown(handle: *mut c_void) -> *mut c_char {
    ffi_response(|| unsafe {
        with_host(handle, |host| {
            host.execute(
                Owner::from_authenticated_transport([0; 32]),
                Command::Tick(TickReason::Shutdown),
            )
        })
    })
}
/// # Safety
/// Handle must come from this library. Only the native host sends events.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_lifecycle_begin(handle: *mut c_void, event: u8) -> *mut c_char {
    ffi_response(|| unsafe { with_host(handle, |host| host.lifecycle_begin(event)) })
}
/// # Safety
/// Handle must come from this library and remain live for this call.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_lifecycle_poll(handle: *mut c_void) -> *mut c_char {
    ffi_response(|| unsafe { with_host(handle, HostState::lifecycle_poll) })
}
/// Frees a fixture host after the harness has stopped all calls and timers.
/// Production hosts deliberately keep their recovery ledger for process lifetime.
/// # Safety
/// Handle must be null or exclusively owned from this library, never previously
/// freed; the caller must finish disconnect/shutdown recovery before disposal.
#[cfg(feature = "test-harness")]
#[no_mangle]
pub unsafe extern "C" fn mf_helper_test_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: the harness gives exclusive ownership back exactly once.
        drop(unsafe { Box::from_raw(handle.cast::<Host>()) });
    }
}
#[cfg(feature = "test-harness")]
#[no_mangle]
pub extern "C" fn mf_helper_test_create(failures: u8) -> *mut c_void {
    let mut host = HostState::new(true);
    host.engine = Coordinator::new(
        GuardedBackend {
            backend: Backend::Fake {
                state: HardwareState {
                    mode: Mode::Automatic,
                    target_rpm: Some(1500.0),
                },
                automatic_failures: failures,
            },
            origin: host.origin,
            plan_deadline: host.plan_deadline.clone(),
        },
        OsTokens,
        HostClock(host.origin),
    );
    Box::into_raw(Box::new(Host {
        state: Mutex::new(host),
    }))
    .cast()
}
/// # Safety
/// Only pass a non-freed string returned by this library, or NULL.
#[no_mangle]
pub unsafe extern "C" fn mf_helper_free(response: *mut c_char) {
    if !response.is_null() {
        // SAFETY: Caller transfers this exact CString allocation back once.
        drop(unsafe { CString::from_raw(response) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(command: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"command":command})).unwrap()
    }
    #[test]
    fn schema_is_closed_and_rejects_client_identity_clock_or_host_events() {
        for bad in [
            json!({"version":1,"owner":"forged","command":{"op":"enumerate"}}),
            json!({"version":1,"command":{"op":"enumerate","pid":1}}),
            json!({"version":1,"command":{"op":"tick"}}),
            json!({"version":1,"command":{"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5,"seq":1,"now_ms":0}}),
            json!({"version":1,"command":{"op":"capabilities","fan":65536}}),
        ] {
            assert!(parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        assert!(parse(br#"{"version":1,"version":1,"command":{"op":"enumerate"}}"#).is_err());
        assert!(parse(br#"{"version":1,"command":{"op":"enumerate","op":"enumerate"}}"#).is_err());
    }
    #[test]
    fn exact_u64_sequences_and_size_are_checked_before_dispatch() {
        for seq in [0, 1, u64::MAX] {
            assert!(parse(&request(
                json!({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5,"seq":seq})
            ))
            .is_ok());
        }
        for bad in ["-1", "18446744073709551616", "1.5", "1e3", "null", "\"1\""] {
            let raw = format!(
                r#"{{"version":1,"command":{{"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5,"seq":{bad}}}}}"#
            );
            assert!(parse(raw.as_bytes()).is_err());
        }
        assert!(matches!(
            parse(&vec![b' '; MAX_REQUEST_BYTES + 1]),
            Err("request_too_large")
        ));
    }
    #[test]
    fn inspection_schema_is_closed_and_nonhardware_backends_are_rejected() {
        assert!(matches!(
            parse(&request(json!({"op":"inspect_hardware"}))),
            Ok(WireCommand::InspectHardware)
        ));
        for extra in [
            json!({"op":"inspect_hardware","fan":0}),
            json!({"op":"inspect_hardware","profile":"verified"}),
        ] {
            assert!(parse(&request(extra)).is_err());
        }
        for fake in [false, true] {
            let mut host = HostState::new(fake);
            let response = host.request(
                Owner::from_authenticated_transport([1; 32]),
                &request(json!({"op":"inspect_hardware"})),
            );
            assert_eq!(response["error"]["code"], "not_supported");
            assert_eq!(response["hardware_reads"], false);
            assert!(response["hardware_write_attempts"].is_null());
            assert!(response.get("result").is_none());
        }
    }
    #[test]
    fn smc_open_failure_remains_structured_and_never_becomes_fake() {
        let mut host = HostState::from_smc_result(Err(AdapterError::Unavailable {
            reason: "injected_open_failure".into(),
        }));
        for command in [
            json!({"op":"enumerate"}),
            json!({"op":"inspect_hardware"}),
            json!({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":1}),
        ] {
            let response = host.request(
                Owner::from_authenticated_transport([1; 32]),
                &request(command),
            );
            assert_eq!(response["backend"], "unavailable");
            assert_eq!(response["backend_open_error"]["kind"], "unavailable");
            assert_eq!(
                response["backend_open_error"]["reason"],
                "injected_open_failure"
            );
            assert_eq!(response["hardware_reads"], false);
            assert_eq!(response["hardware_write_enabled"], false);
            assert!(response["hardware_write_attempts"].is_null());
            assert_eq!(response["ok"], false);
            assert!(host.engine.statuses().is_empty());
        }
    }
    #[test]
    fn target_updates_keep_secret_sequence_and_schema_boundaries() {
        let mut host = HostState::new(true);
        let owner = Owner::from_authenticated_transport([1; 32]);
        let other = Owner::from_authenticated_transport([2; 32]);
        let acquired = host.request(
            owner,
            &request(json!({"op":"acquire","fan":0,
            "rpm":3000,"ttl_ms":5000,"seq":1})),
        );
        assert_eq!(acquired["ok"], true);
        let secret = acquired["result"]["token"].as_str().unwrap();
        let update =
            json!({"op":"set_target","fan":0,"token":secret,"rpm":3500,"ttl_ms":5000,"seq":2});
        assert_eq!(
            host.request(other, &request(update.clone()))["error"]["code"],
            "owner_conflict"
        );
        let mut forged = update.clone();
        forged["token"] = json!("0".repeat(64));
        assert_eq!(
            host.request(owner, &request(forged))["error"]["code"],
            "invalid_token"
        );
        for field in ["owner", "mode", "key", "profile", "now_ms"] {
            let mut extra = update.clone();
            extra[field] = json!(0);
            assert_eq!(
                host.request(owner, &request(extra))["error"]["code"],
                "invalid_schema"
            );
        }
        let changed = host.request(owner, &request(update.clone()));
        assert_eq!(changed["ok"], true);
        assert_eq!(changed["result"]["token"], secret);
        assert_eq!(changed["result"]["target_rpm"], 3500.0);
        assert_eq!(changed["result"]["seq"], 2);
        assert_eq!(changed["hardware_write_enabled"], false);
        assert_eq!(
            host.request(owner, &request(update))["error"]["code"],
            "invalid_sequence"
        );
        let release = host.request(
            owner,
            &request(json!({"op":"release","fan":0,"token":secret,"seq":3})),
        );
        assert_eq!(release["result"]["kind"], "automatic_confirmed");
        let after = host.request(
            owner,
            &request(
                json!({"op":"set_target","fan":0,"token":secret,"rpm":4000,"ttl_ms":100,"seq":4}),
            ),
        );
        assert_eq!(after["error"]["code"], "no_active_lease");
    }

    #[test]
    fn capabilities_response_includes_all_control_admission_facts() {
        let mut host = HostState::new(true);
        let response = host.request(
            Owner::from_authenticated_transport([1; 32]),
            &request(json!({"op":"capabilities","fan":0})),
        );
        assert_eq!(response["result"]["model"], "Verified");
        assert_eq!(response["result"]["mode_control"], "PerFan");
        assert_eq!(response["result"]["stages_target_while_automatic"], true);
        assert_eq!(response["hardware_write_enabled"], false);
    }
    #[test]
    fn secrets_require_exact_lowercase_32_byte_encoding() {
        for bad in [
            "".to_owned(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "z".repeat(64),
            "é".repeat(32),
        ] {
            assert!(token(&bad).is_err());
        }
        assert_eq!(
            token_text(&token(&"ab".repeat(32)).unwrap()),
            "ab".repeat(32)
        );
    }
    #[test]
    fn explicit_unavailable_and_fake_never_claim_hardware_control() {
        for fake in [false, true] {
            let mut host = HostState::new(fake);
            let result = host.request(
                Owner::from_authenticated_transport([1; 32]),
                &request(json!({"op":"enumerate"})),
            );
            assert_eq!(result["dry_run"], true);
            assert_eq!(result["hardware_write_enabled"], false);
            assert_eq!(result["ok"], fake);
        }
    }
    #[test]
    fn malformed_request_cannot_become_a_control_command() {
        let mut host = HostState::new(true);
        let owner = Owner::from_authenticated_transport([1; 32]);
        for raw in [b"".as_slice(), b"{}", b"null", b"[]", b"\xff", b"{} {}"] {
            let result = host.request(owner, raw);
            assert_eq!(result["ok"], false);
            assert_eq!(result["error"]["code"], "invalid_schema");
            assert!(host.engine.statuses().is_empty());
        }
    }
    #[test]
    fn ffi_rejections_always_keep_version_and_dry_run_markers() {
        let handle = mf_helper_create(1);
        let owner = [1_u8; 32];
        let oversized = vec![b' '; MAX_REQUEST_BYTES + 1];
        // SAFETY: Handles/buffers are live, and each NULL case is explicitly
        // handled before dereferencing. No request starts a lease here.
        let rejected = unsafe {
            [
                mf_helper_request(handle, owner.as_ptr(), std::ptr::null(), 0),
                mf_helper_request(handle, std::ptr::null(), b"{}".as_ptr(), 2),
                mf_helper_request(std::ptr::null_mut(), owner.as_ptr(), b"{}".as_ptr(), 2),
                mf_helper_request(handle, owner.as_ptr(), oversized.as_ptr(), oversized.len()),
            ]
        };
        for pointer in rejected {
            // SAFETY: Each returned CString is read and returned exactly once.
            let value: Value = unsafe {
                serde_json::from_slice(std::ffi::CStr::from_ptr(pointer).to_bytes()).unwrap()
            };
            unsafe {
                mf_helper_free(pointer);
            }
            assert_eq!(value["version"], 1);
            assert_eq!(value["dry_run"], true);
            assert_eq!(value["hardware_write_enabled"], false);
            assert_eq!(value["ok"], false);
        }
        // SAFETY: This test owns the only handle and all calls have completed.
        drop(unsafe { Box::from_raw(handle.cast::<Host>()) });
    }
    #[test]
    fn trusted_sleep_and_wake_revoke_and_never_resume_old_targets() {
        let mut host = HostState::new(true);
        let owner = Owner::from_authenticated_transport([1; 32]);
        let acquired = host.request(
            owner,
            &request(json!({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":1})),
        );
        let secret = acquired["result"]["token"].as_str().unwrap();
        let sleep = host.lifecycle_begin(1);
        assert_eq!(sleep["recovery_confirmed"], true);
        assert_eq!(sleep["lifecycle"], "sleep_locked");
        let renew = request(json!({"op":"renew","fan":0,"token":secret,"ttl_ms":5000,"seq":2}));
        assert_eq!(host.request(owner, &renew)["error"]["code"], "suspended");
        let wake = host.lifecycle_begin(2);
        assert_eq!(wake["lifecycle"], "wake_requires_new_session");
        assert_eq!(wake["recovery_confirmed"], true);
        assert_eq!(
            host.request(
                owner,
                &request(json!({"op":"acquire","fan":0,"rpm":4000,"ttl_ms":5000,"seq":1}))
            )["error"]["code"],
            "suspended"
        );
        assert_eq!(host.request(owner, &renew)["error"]["code"], "suspended");
    }
    #[test]
    fn unexpected_wake_also_locks_and_invalid_host_event_cannot_revive_lease() {
        let mut host = HostState::new(true);
        let owner = Owner::from_authenticated_transport([1; 32]);
        host.request(
            owner,
            &request(json!({"op":"acquire","fan":0,"rpm":3000,"ttl_ms":5000,"seq":1})),
        );
        assert_eq!(
            host.lifecycle_begin(255)["error"]["code"],
            "invalid_host_event"
        );
        assert!(matches!(
            host.engine.status(FanId(0)).unwrap().phase,
            Phase::Active { .. }
        ));
        assert_eq!(host.lifecycle_begin(2)["recovery_confirmed"], true);
        assert_eq!(host.lifecycle_begin(3)["lifecycle"], "shutting_down");
        assert_eq!(host.lifecycle_poll()["recovery_pending"], false);
    }
}

#[cfg(test)]
mod control_backend_tests {
    use super::*;
    #[derive(Default)]
    struct MockActuator {
        calls: Vec<String>,
    }
    impl Actuator for MockActuator {
        fn enumerate(&mut self) -> Result<Vec<FanId>, ActuatorError> {
            self.calls.push("enumerate".into());
            Ok(vec![FanId(0), FanId(1)])
        }
        fn capabilities(&mut self, fan: FanId) -> Result<Capabilities, ActuatorError> {
            self.calls.push(format!("capabilities:{}", fan.0));
            Err(ActuatorError::Io)
        }
        fn read_state(&mut self, fan: FanId) -> Result<HardwareState, ActuatorError> {
            self.calls.push(format!("state:{}", fan.0));
            Ok(HardwareState {
                mode: Mode::Automatic,
                target_rpm: Some(2000.0),
            })
        }
        fn write_target(&mut self, fan: FanId, rpm: f64) -> Result<(), ActuatorError> {
            self.calls.push(format!("target:{}:{rpm}", fan.0));
            Ok(())
        }
        fn write_mode(&mut self, fan: FanId, mode: WriteMode) -> Result<(), ActuatorError> {
            self.calls.push(format!("mode:{}:{mode:?}", fan.0));
            Ok(())
        }
    }
    #[test]
    fn control_forwards_all_operations_for_second_fan_and_propagates_errors() {
        let adapter = Arc::new(Mutex::new(MockActuator::default()));
        let mut backend = Backend::SmcControl(adapter.clone());
        assert_eq!(backend.enumerate(), Ok(vec![FanId(0), FanId(1)]));
        assert_eq!(backend.capabilities(FanId(1)), Err(ActuatorError::Io));
        assert_eq!(backend.read_state(FanId(1)).unwrap().mode, Mode::Automatic);
        backend.write_target(FanId(1), 3000.0).unwrap();
        backend.write_mode(FanId(1), WriteMode::Manual).unwrap();
        backend.write_mode(FanId(1), WriteMode::Automatic).unwrap();
        assert_eq!(
            adapter.lock().unwrap().calls,
            [
                "enumerate",
                "capabilities:1",
                "state:1",
                "target:1:3000",
                "mode:1:Manual",
                "mode:1:Automatic"
            ]
        );
    }
    #[test]
    fn readonly_backend_cannot_forward_writes_even_to_a_writable_mock() {
        let adapter = Arc::new(Mutex::new(MockActuator::default()));
        let mut backend = Backend::SmcReadonly(adapter.clone());
        for fan in [FanId(0), FanId(1)] {
            assert_eq!(
                backend.write_target(fan, 3000.0),
                Err(ActuatorError::Unsupported)
            );
            for mode in [WriteMode::Manual, WriteMode::Automatic] {
                assert_eq!(
                    backend.write_mode(fan, mode),
                    Err(ActuatorError::Unsupported)
                );
            }
        }
        assert!(adapter.lock().unwrap().calls.is_empty());
    }
    #[test]
    fn control_keeps_deadline_fence_and_automatic_recovery() {
        let adapter = Arc::new(Mutex::new(MockActuator::default()));
        let mut backend = GuardedBackend {
            backend: Backend::SmcControl(adapter.clone()),
            origin: Instant::now(),
            plan_deadline: Arc::new(AtomicU64::new(0)),
        };
        assert_eq!(
            backend.write_target(FanId(1), 3000.0),
            Err(ActuatorError::Timeout)
        );
        assert_eq!(
            backend.write_mode(FanId(1), WriteMode::Manual),
            Err(ActuatorError::Timeout)
        );
        backend.write_mode(FanId(1), WriteMode::Automatic).unwrap();
        assert_eq!(adapter.lock().unwrap().calls, ["mode:1:Automatic"]);
    }
    #[test]
    fn both_smc_open_error_paths_are_unavailable_and_never_spawn_sampler() {
        for build in [
            HostState::from_smc_result,
            HostState::from_smc_control_result,
        ] {
            let host = build(Err(AdapterError::Unavailable {
                reason: "mock open failure".into(),
            }));
            assert_eq!(host.execution, HostExecution::BackendOpenFailed);
            assert!(!host.hardware_reads);
            assert!(host.smc.is_none());
            assert!(!host.sampler.is_alive());
            assert!(host.backend_open_error.is_some());
        }
    }
    #[test]
    fn invalid_handle_and_panics_do_not_offer_control_fingerprint() {
        let error = unsafe { with_host(std::ptr::null_mut(), |_| panic!("must not be called")) };
        assert!(error.get("control").is_none());
        assert_eq!(error["error"]["code"], "invalid_handle");
        let response = ffi_response(|| panic!("injected ffi panic"));
        let error: Value = unsafe {
            serde_json::from_slice(std::ffi::CStr::from_ptr(response).to_bytes()).unwrap()
        };
        unsafe { mf_helper_free(response) };
        assert!(error.get("control").is_none());
        assert_eq!(error["error"]["code"], "internal_failure");
    }
}
