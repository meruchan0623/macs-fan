//! Authenticated host plan lifecycle. Wire data never supplies samples or time.
use super::*;
use macs_fan_control_runtime::{FanOutcome, FanPlan, Plan, SessionPhase, Strategy};

const LEASE_TTL_MS: u64 = 1000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireEntry {
    fan: u16,
    strategy: WireStrategy,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireStrategy {
    Automatic {},
    Fixed {
        rpm: f64,
    },
    Sensor {
        sensor_id: String,
        min_rpm: f64,
        max_rpm: f64,
        lower_temp: f64,
        upper_temp: f64,
    },
}
pub(super) enum PlanRequest {
    Start {
        entries: Vec<WireEntry>,
        ttl_ms: u64,
        seq: u64,
    },
    Renew {
        token: LeaseToken,
        ttl_ms: u64,
        seq: u64,
    },
    Stop {
        token: LeaseToken,
        seq: u64,
    },
    Status,
}
pub(super) struct PlanAuthorization {
    pub owner: Owner,
    token: LeaseToken,
    seq: u64,
    expires_at_ms: u64,
}

pub(super) fn runtime_error(error: &macs_fan_control_runtime::Error) -> &'static str {
    use macs_fan_control_runtime::Error as E;
    match error {
        E::Core(error) => error_code(error),
        E::Busy => "plan_busy",
        E::InvalidPlan => "invalid_plan",
        E::InvalidSensorId => "invalid_sensor_id",
        E::DuplicateSample => "duplicate_sample",
        E::InvalidConfig(_) => "invalid_policy_config",
        E::Sensor { .. } => "sensor_fault",
        E::SessionLost(_) => "no_active_lease",
        E::SequenceExhausted => "sequence_exhausted",
        E::UnexpectedResponse => "unexpected_response",
        E::InvalidDirectCommand => "invalid_host_command",
    }
}
fn deadline(now: u64, ttl: u64) -> Result<u64, &'static str> {
    if ttl == 0 || ttl > MAX_TTL_MS {
        return Err("invalid_ttl");
    }
    now.checked_add(ttl).ok_or("deadline_overflow")
}
fn plan(entries: Vec<WireEntry>, sampler: &sensors::Sampler) -> Result<Plan, &'static str> {
    if entries.is_empty() || entries.len() > 16 {
        return Err("invalid_plan");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut fans = Vec::new();
    for entry in entries {
        if !seen.insert(entry.fan) {
            return Err("invalid_plan");
        }
        let strategy = match entry.strategy {
            WireStrategy::Automatic {} => Strategy::Automatic,
            WireStrategy::Fixed { rpm } => Strategy::Fixed { rpm },
            WireStrategy::Sensor {
                sensor_id,
                min_rpm,
                max_rpm,
                lower_temp,
                upper_temp,
            } => {
                if sensor_id.is_empty()
                    || sensor_id.len() > 256
                    || sensor_id.chars().any(char::is_control)
                {
                    return Err("invalid_sensor_id");
                }
                let config = macs_fan_control_runtime::policy::Config {
                    min_rpm,
                    max_rpm,
                    low_celsius: lower_temp,
                    high_celsius: upper_temp,
                    sample_timeout_ms: 1000,
                    descent_hold_ms: 2000,
                    descent_rpm_per_second: 100.0,
                    recovery_samples: 2,
                };
                config.validate().map_err(|_| "invalid_policy_config")?;
                // Supports is a private host admission decision. A decoded SMC
                // candidate or frontend quality claim never creates this right.
                if !sampler.supports(&sensor_id) {
                    return Err("sensor_source_unavailable");
                }
                Strategy::Sensor {
                    sensor: macs_fan_control_runtime::SensorId(sensor_id),
                    config,
                }
            }
        };
        fans.push(FanPlan {
            fan: FanId(entry.fan),
            strategy,
        });
    }
    Ok(Plan {
        entries: fans,
        ttl_ms: LEASE_TTL_MS,
    })
}

impl HostState {
    /// Consume only completed local batches without waiting for I/O or a sampler lock.
    /// The worker and runtime share origin; never relabel cached values as new.
    fn local_samples(
        &mut self,
        ids: &std::collections::BTreeSet<String>,
    ) -> Option<(u64, Vec<macs_fan_control_runtime::SensorReading>)> {
        if ids.is_empty() {
            return Some((0, vec![]));
        }
        if !self.sampler.is_alive() {
            self.sensor_batch = None;
            return None;
        }
        if let Some(batch) = self.sampler.latest() {
            self.sensor_batch = Some(batch);
        }
        if !self.sampler.is_alive() {
            self.sensor_batch = None;
            return None;
        }
        let batch = self.sensor_batch.as_ref()?;
        let readings: Vec<_> = batch
            .readings
            .iter()
            .filter(|r| ids.contains(&r.sensor.0))
            .cloned()
            .collect();
        if readings.len() != ids.len()
            || batch.sequence == 0
            || readings
                .iter()
                .any(|r| r.sample.sampled_at_ms != batch.sampled_at_ms)
        {
            return None;
        }
        Some((batch.sequence, readings))
    }

    pub(super) fn plan_error(&self, code: &'static str) -> Value {
        let mut reply = self.wrap(Err(Error::NoActiveLease));
        reply["error"] = json!({"code":code});
        reply["plan"] = self.plan_snapshot();
        self.attach_execution(&mut reply, true);
        reply
    }
    fn plan_snapshot(&self) -> Value {
        let snapshot = self.engine.snapshot();
        let phase = match snapshot.phase {
            SessionPhase::Idle => "idle",
            SessionPhase::Preparing => "preparing",
            SessionPhase::Running => "running",
            SessionPhase::Stopped => "stopped",
        };
        let fans: Vec<_> = snapshot
            .fans
            .into_iter()
            .map(|(fan, outcome)| {
                let mut result = json!({"fan":fan.0});
                let state = match outcome {
                    FanOutcome::Unobserved => "unobserved",
                    FanOutcome::AutomaticConfirmed => "automatic_confirmed",
                    FanOutcome::RecoveryPending => "recovery_pending",
                    FanOutcome::RecoveryExhausted => "recovery_exhausted",
                    FanOutcome::Yielded => "yielded",
                    FanOutcome::Unknown => "unknown",
                    FanOutcome::Active {
                        target_rpm,
                        expires_at_ms,
                    } => {
                        result["target_rpm"] = json!(target_rpm);
                        result["lease_expires_at_ms"] = json!(expires_at_ms);
                        "active"
                    }
                };
                result["state"] = json!(state);
                result
            })
            .collect();
        json!({"phase":phase, "fans":fans, "watchdog_expirations":self.plan_watchdog_expirations,
        "authorized":self.plan_authorization.is_some(),
        "consent_expires_at_ms":self.plan_authorization.as_ref().map(|a| a.expires_at_ms),
        "lease_ttl_ms":LEASE_TTL_MS, "authorization_end":self.authorization_end,
        "error":snapshot.last_error.as_ref().map(runtime_error),
        "stop_reason":snapshot.stop_reason.map(|r| match r {
            StopReason::Explicit => "explicit", StopReason::Disconnected => "disconnected",
            StopReason::Sleep => "sleep", StopReason::Shutdown => "shutdown",
        })})
    }
    fn plan_reply(&self) -> Value {
        let mut reply = self.wrap(Ok(Response::Statuses(self.engine.statuses())));
        reply["result"] = json!({"kind":"plan", "plan":self.plan_snapshot()});
        self.attach_execution(&mut reply, true);
        reply
    }
    pub(super) fn refresh_reply(&mut self, previous: Value, requires_authorization: bool) -> Value {
        self.expire_authorization();
        if requires_authorization && previous["ok"] == true && self.plan_authorization.is_none() {
            return self.plan_error("no_active_plan");
        }
        let mut reply = self.wrap(Ok(Response::Statuses(self.engine.statuses())));
        for field in ["ok", "result", "error"] {
            reply.as_object_mut().unwrap().remove(field);
            if let Some(value) = previous.get(field) {
                reply[field] = value.clone();
            }
        }
        let is_plan = previous.get("plan").is_some() || previous["result"]["kind"] == "plan";
        self.attach_execution(&mut reply, is_plan);
        if previous.get("plan").is_some() {
            reply["plan"] = self.plan_snapshot();
        }
        if reply["result"]["kind"] == "plan" {
            // Never publish a start secret after the final consent audit revoked it.
            if reply["result"].get("token").is_some() && self.plan_authorization.is_none() {
                return self.plan_error("no_active_plan");
            }
            reply["result"]["plan"] = self.plan_snapshot();
        } else if reply["result"]["kind"] == "statuses" {
            reply["result"] = response_json(Response::Statuses(self.engine.statuses()));
        } else if reply["result"]["kind"] == "lease" {
            let fan = reply["result"]["fan"]
                .as_u64()
                .and_then(|v| u16::try_from(v).ok());
            let live = fan
                .and_then(|fan| self.engine.status(FanId(fan)))
                .is_some_and(|status| {
                    matches!(status.phase, Phase::Active { seq, expires_at_ms, .. }
                    if reply["result"]["seq"].as_u64() == Some(seq)
                    && reply["result"]["expires_at_ms"].as_u64() == Some(expires_at_ms)
                    && expires_at_ms > self.now())
                });
            if !live {
                let _ = self.engine.watchdog();
                return self.wrap(Err(Error::NoActiveLease));
            }
        }
        reply
    }
    fn expire_authorization(&mut self) {
        if self
            .plan_authorization
            .as_ref()
            .is_some_and(|a| self.now() >= a.expires_at_ms)
        {
            self.plan_authorization = None;
            self.authorization_end = Some("expired");
            let _ = self.engine.stop(StopReason::Disconnected);
        }
    }
    /// The native independent timer calls this with advance=true. Requests only
    /// audit, so client traffic cannot masquerade as an independent scheduler.
    pub(super) fn maintain_plan(&mut self, advance: bool) {
        self.expire_authorization();
        let _ = self.engine.watchdog();
        self.expire_authorization();
        if advance && self.plan_authorization.is_some() {
            let ids = self.plan_sensor_ids.clone();
            if ids.is_empty() {
                let _ = self.engine.step(&[]);
            } else {
                match self.local_samples(&ids) {
                    Some((sequence, readings)) if sequence > self.plan_sample_sequence => {
                        self.plan_sample_sequence = sequence;
                        let _ = self.engine.step(&readings);
                    }
                    Some((sequence, _)) if sequence < self.plan_sample_sequence => {
                        let _ = self.engine.step(&[]);
                    }
                    None if !self.sampler.is_alive() => {
                        let _ = self.engine.step(&[]);
                    }
                    // Same cached batch / no new result: watchdog only. The
                    // runtime's sample deadline still expires independently.
                    _ => {}
                }
            }
            self.expire_authorization();
        }
        if self.plan_authorization.is_some()
            && self.engine.snapshot().phase == SessionPhase::Stopped
        {
            self.plan_authorization = None;
            self.authorization_end = Some("fault");
        }
        if self.plan_authorization.is_none() {
            self.plan_sensor_ids.clear();
        }
    }
    fn authorize(&self, owner: Owner, token: &LeaseToken, seq: u64) -> Result<(), &'static str> {
        let auth = self.plan_authorization.as_ref().ok_or("no_active_plan")?;
        if auth.owner != owner {
            return Err("owner_conflict");
        }
        let different = auth
            .token
            .transport_bytes()
            .iter()
            .zip(token.transport_bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b));
        if different != 0 {
            return Err("invalid_token");
        }
        if seq <= auth.seq {
            return Err("invalid_sequence");
        }
        Ok(())
    }
    pub(super) fn plan_request(&mut self, owner: Owner, command: PlanRequest) -> Value {
        match command {
            PlanRequest::Status => self.plan_reply(),
            PlanRequest::Start {
                entries,
                ttl_ms,
                seq,
            } => {
                if !self.execution.permits_plan() {
                    return self.plan_error(self.execution.admission());
                }
                if self.plan_authorization.is_some() {
                    return self.plan_error("plan_busy");
                }
                if seq == 0 {
                    return self.plan_error("invalid_sequence");
                }
                let expires = match deadline(self.now(), ttl_ms) {
                    Ok(v) => v,
                    Err(e) => return self.plan_error(e),
                };
                let plan = match plan(entries, &self.sampler) {
                    Ok(v) => v,
                    Err(e) => return self.plan_error(e),
                };
                let sensor_ids: std::collections::BTreeSet<_> = plan
                    .entries
                    .iter()
                    .filter_map(|e| match &e.strategy {
                        Strategy::Sensor { sensor, .. } => Some(sensor.0.clone()),
                        _ => None,
                    })
                    .collect();
                let (sample_sequence, samples) = match self.local_samples(&sensor_ids) {
                    Some(v) => v,
                    None => return self.plan_error("sensor_source_unavailable"),
                };
                if self.issued_plan_tokens.len() >= 65_536 {
                    return self.plan_error("entropy_unavailable");
                }
                let bytes = match OsTokens.next_token() {
                    Ok(v) if v != [0; 32] && !self.issued_plan_tokens.contains(&v) => v,
                    _ => return self.plan_error("entropy_unavailable"),
                };
                self.plan_deadline.store(expires, Ordering::SeqCst);
                match self.engine.start(owner, plan, &samples) {
                    Err(error) => self.plan_error(runtime_error(&error)),
                    Ok(_) => {
                        self.plan_sensor_ids = sensor_ids;
                        self.plan_sample_sequence = sample_sequence;
                        self.issued_plan_tokens.insert(bytes);
                        let secret = LeaseToken::from_transport_bytes(bytes);
                        self.plan_authorization = Some(PlanAuthorization {
                            owner,
                            token: secret.clone(),
                            seq,
                            expires_at_ms: expires,
                        });
                        self.authorization_end = None;
                        self.maintain_plan(false);
                        if self.plan_authorization.is_none() {
                            return self.plan_error("no_active_plan");
                        }
                        let mut reply = self.plan_reply();
                        reply["result"]["token"] = json!(token_text(&secret));
                        reply
                    }
                }
            }
            PlanRequest::Renew { token, ttl_ms, seq } => {
                if let Err(error) = self.authorize(owner, &token, seq) {
                    return self.plan_error(error);
                }
                let now = self.now();
                if now >= self.plan_authorization.as_ref().unwrap().expires_at_ms {
                    self.expire_authorization();
                    return self.plan_error("no_active_plan");
                }
                let expires = match deadline(now, ttl_ms) {
                    Ok(v) => v,
                    Err(e) => return self.plan_error(e),
                };
                let auth = self.plan_authorization.as_mut().unwrap();
                auth.seq = seq;
                auth.expires_at_ms = expires;
                self.plan_deadline.store(expires, Ordering::SeqCst);
                self.plan_reply()
            }
            PlanRequest::Stop { token, seq } => {
                if let Err(error) = self.authorize(owner, &token, seq) {
                    return self.plan_error(error);
                }
                self.plan_authorization = None;
                self.authorization_end = Some("stopped");
                match self.engine.stop(StopReason::Explicit) {
                    Ok(_) => self.plan_reply(),
                    Err(error) => self.plan_error(runtime_error(&error)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(n: u8) -> Owner {
        Owner::from_authenticated_transport([n; 32])
    }
    fn call(h: &mut HostState, who: u8, command: Value) -> Value {
        h.request(
            owner(who),
            &serde_json::to_vec(&json!({"version":1,"command":command})).unwrap(),
        )
    }
    fn start() -> Value {
        json!({"op":"start_plan","entries":[{"fan":0,"strategy":{"type":"fixed","rpm":3000}}],"ttl_ms":5000,"seq":1})
    }
    #[test]
    fn owner_secret_sequence_and_status_are_separate_from_core_leases() {
        let mut h = HostState::new(true);
        let r = call(&mut h, 1, start());
        assert_eq!(r["ok"], true);
        let token = r["result"]["token"].as_str().unwrap().to_owned();
        assert_eq!(r["result"]["plan"]["phase"], "running");
        assert_eq!(r["result"]["plan"]["fans"][0]["target_rpm"], 3000.0);
        let status = call(&mut h, 2, json!({"op":"plan_status"}));
        assert!(!status.to_string().contains(&token));
        let renew = json!({"op":"renew_plan","token":token,"ttl_ms":5000,"seq":2});
        assert_eq!(
            call(&mut h, 2, renew.clone())["error"]["code"],
            "owner_conflict"
        );
        assert_eq!(
            call(
                &mut h,
                1,
                json!({"op":"renew_plan","token":"00".repeat(32),"ttl_ms":5000,"seq":2})
            )["error"]["code"],
            "invalid_token"
        );
        assert_eq!(call(&mut h, 1, renew.clone())["ok"], true);
        assert_eq!(call(&mut h, 1, renew)["error"]["code"], "invalid_sequence");
        assert_eq!(
            call(
                &mut h,
                1,
                json!({"op":"acquire","fan":0,"rpm":4000,"ttl_ms":5000,"seq":1})
            )["error"]["code"],
            "plan_busy"
        );
        h.execute(owner(2), Command::Tick(TickReason::Disconnected(owner(2))));
        assert!(h.plan_authorization.is_some());
        h.watchdog_tick();
        let stopped = call(&mut h, 1, json!({"op":"stop_plan","token":token,"seq":3}));
        assert_eq!(
            stopped["result"]["plan"]["fans"][0]["state"],
            "automatic_confirmed"
        );
        assert_eq!(stopped["result"]["plan"]["authorized"], false);
        assert_eq!(
            call(
                &mut h,
                1,
                json!({"op":"renew_plan","token":token,"ttl_ms":5000,"seq":4})
            )["error"]["code"],
            "no_active_plan"
        );
    }
    #[test]
    fn expired_consent_cannot_be_revived_by_request_or_timer() {
        for timer in [true, false] {
            let mut h = HostState::new(true);
            let r = call(&mut h, 1, start());
            let token = r["result"]["token"].clone();
            h.plan_authorization.as_mut().unwrap().expires_at_ms = 0;
            if timer {
                h.watchdog_tick();
            }
            let r = call(
                &mut h,
                1,
                json!({"op":"renew_plan","token":token,"ttl_ms":5000,"seq":2}),
            );
            assert_eq!(r["error"]["code"], "no_active_plan");
            assert_eq!(r["plan"]["authorization_end"], "expired");
            assert_eq!(r["plan"]["fans"][0]["state"], "automatic_confirmed");
            h.watchdog_tick();
            assert!(h.plan_authorization.is_none());
        }
    }
    #[test]
    fn direct_plan_interlock_and_lifecycle_use_same_ledger() {
        let mut h = HostState::new(true);
        let r = call(
            &mut h,
            1,
            json!({"op":"acquire","fan":0,"rpm":4000,"ttl_ms":5000,"seq":1}),
        );
        assert_eq!(call(&mut h, 2, start())["error"]["code"], "plan_busy");
        assert!(h.plan_authorization.is_none());
        call(
            &mut h,
            1,
            json!({"op":"release","fan":0,"token":r["result"]["token"],"seq":2}),
        );
        assert_eq!(call(&mut h, 2, start())["ok"], true);
        h.execute(owner(2), Command::Tick(TickReason::Disconnected(owner(2))));
        assert!(h.plan_authorization.is_none());
        assert_eq!(call(&mut h, 1, start())["ok"], true);
        let r = h.lifecycle_begin(1);
        assert_eq!(r["recovery_confirmed"], true);
        assert!(h.plan_authorization.is_none());
        assert_eq!(call(&mut h, 1, start())["error"]["code"], "suspended");
    }
    #[test]
    fn unsupported_sensor_or_untrusted_fields_never_partially_apply() {
        let sensor = json!({"fan":0,"strategy":{"type":"sensor","sensor_id":"smc:TCMb","min_rpm":1000,"max_rpm":5000,"lower_temp":40,"upper_temp":80}});
        let mut h = HostState::new(true);
        let mut command = start();
        command["entries"] = json!([{"fan":1,"strategy":{"type":"fixed","rpm":3000}},sensor]);
        assert_eq!(
            call(&mut h, 1, command)["error"]["code"],
            "sensor_source_unavailable"
        );
        assert!(h.engine.statuses().is_empty());
        for field in ["samples", "owner", "now_ms", "quality", "capabilities"] {
            let mut command = start();
            command[field] = json!("forged");
            assert_eq!(call(&mut h, 1, command)["error"]["code"], "invalid_schema");
        }
        for entries in [
            json!([]),
            json!([{"fan":0,"strategy":{"type":"fixed","rpm":3000}},{"fan":0,"strategy":{"type":"automatic"}}]),
        ] {
            let mut command = start();
            command["entries"] = entries;
            assert_eq!(call(&mut h, 1, command)["error"]["code"], "invalid_plan");
        }
        assert!(h.engine.statuses().is_empty());
    }
    #[test]
    fn unavailable_backend_and_invalid_consent_do_not_authorize_a_plan() {
        let mut h = HostState::new(false);
        assert_eq!(call(&mut h, 1, start())["ok"], false);
        assert!(h.plan_authorization.is_none());
        let mut h = HostState::new(true);
        for ttl in [0, 5001] {
            let mut c = start();
            c["ttl_ms"] = json!(ttl);
            assert_eq!(call(&mut h, 1, c)["error"]["code"], "invalid_ttl");
        }
        let mut c = start();
        c["seq"] = json!(0);
        assert_eq!(call(&mut h, 1, c)["error"]["code"], "invalid_sequence");
        assert!(h.engine.statuses().is_empty());
    }
    #[test]
    fn expired_write_fence_blocks_new_control_but_permits_automatic_recovery() {
        let mut backend: GuardedBackend = GuardedBackend {
            backend: Backend::Fake {
                state: HardwareState {
                    mode: Mode::Manual,
                    target_rpm: Some(1500.0),
                },
                automatic_failures: 0,
            },
            origin: Instant::now(),
            plan_deadline: Arc::new(AtomicU64::new(0)),
        };
        assert_eq!(
            backend.write_target(FanId(0), 3000.0),
            Err(ActuatorError::Timeout)
        );
        assert_eq!(
            backend.write_mode(FanId(0), WriteMode::Manual),
            Err(ActuatorError::Timeout)
        );
        assert_eq!(
            backend.read_state(FanId(0)).unwrap().target_rpm,
            Some(1500.0)
        );
        assert_eq!(backend.write_mode(FanId(0), WriteMode::Automatic), Ok(()));
        assert_eq!(backend.read_state(FanId(0)).unwrap().mode, Mode::Automatic);
    }
    #[test]
    fn final_response_cannot_publish_expired_authorization_or_old_active_status() {
        let mut h = HostState::new(true);
        let response = call(&mut h, 1, start());
        assert_eq!(response["ok"], true);
        let secret = response["result"]["token"].as_str().unwrap().to_owned();
        h.plan_authorization.as_mut().unwrap().expires_at_ms = 0;
        let final_reply = h.refresh_reply(response, true);
        assert_eq!(final_reply["error"]["code"], "no_active_plan");
        assert!(!final_reply.to_string().contains(&secret));
        assert_eq!(
            final_reply["statuses"][0]["phase"]["state"],
            "automatic_confirmed"
        );
    }
    fn sensor_start() -> Value {
        json!({"op":"start_plan","entries":[{"fan":0,"strategy":{"type":"sensor","sensor_id":"test:temperature","min_rpm":1000,"max_rpm":5000,"lower_temp":40,"upper_temp":80}}],"ttl_ms":5000,"seq":1})
    }
    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "host sensor condition timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn ready(h: &mut HostState) {
        let ids = ["test:temperature".to_owned()].into_iter().collect();
        wait_for(|| h.local_samples(&ids).is_some());
    }
    fn running(h: &mut HostState) {
        wait_for(|| {
            h.watchdog_tick();
            h.engine.snapshot().phase == SessionPhase::Running
        });
    }
    #[test]
    fn sensor_prepares_then_only_timer_with_new_local_sample_applies() {
        let mut h = HostState::new(true);
        ready(&mut h);
        let r = call(&mut h, 1, sensor_start());
        assert_eq!(r["ok"], true);
        assert_eq!(r["result"]["plan"]["phase"], "preparing");
        assert_eq!(r["result"]["plan"]["fans"][0]["state"], "unobserved");
        assert!(h.engine.statuses().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(150));
        let r = call(&mut h, 1, json!({"op":"plan_status"}));
        assert_eq!(r["result"]["plan"]["phase"], "preparing");
        assert!(h.engine.statuses().is_empty());
        running(&mut h);
        assert_eq!(h.plan_snapshot()["fans"][0]["target_rpm"], 3000.0);
        assert_eq!(h.plan_snapshot()["fans"][0]["state"], "active");
        let sequence = h.plan_sample_sequence;
        for _ in 0..5 {
            h.watchdog_tick();
        }
        assert!(h.plan_sample_sequence >= sequence);
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Running);
    }
    #[test]
    fn stop_while_preparing_does_not_fake_automatic_readback_or_restart() {
        let mut h = HostState::new(true);
        ready(&mut h);
        let r = call(&mut h, 1, sensor_start());
        let token = r["result"]["token"].as_str().unwrap();
        let stopped = call(&mut h, 1, json!({"op":"stop_plan","token":token,"seq":2}));
        assert_eq!(stopped["ok"], true);
        assert_eq!(stopped["result"]["plan"]["phase"], "stopped");
        assert_eq!(stopped["result"]["plan"]["fans"][0]["state"], "unobserved");
        assert!(h.engine.statuses().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(120));
        h.watchdog_tick();
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Stopped);
        assert!(h.plan_authorization.is_none());
    }
    #[test]
    fn sampler_failure_revokes_plan_without_client_samples_or_restart() {
        let mut h = HostState::new(true);
        let (sampler, control) = sensors::Sampler::test_controlled(h.origin);
        h.sampler = sampler;
        ready(&mut h);
        assert_eq!(call(&mut h, 1, sensor_start())["ok"], true);
        running(&mut h);
        control.fail();
        wait_for(|| !h.sampler.is_alive());
        h.watchdog_tick();
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Stopped);
        assert!(h.plan_authorization.is_none());
        assert_eq!(h.plan_snapshot()["fans"][0]["state"], "automatic_confirmed");
        control.resume();
        h.watchdog_tick();
        assert_eq!(
            call(&mut h, 1, sensor_start())["error"]["code"],
            "sensor_source_unavailable"
        );
    }
    #[test]
    fn hung_sampler_does_not_block_watchdog_and_stale_sample_stops_plan() {
        let mut h = HostState::new(true);
        let (sampler, control) = sensors::Sampler::test_controlled(h.origin);
        h.sampler = sampler;
        struct Resume(sensors::TestControl);
        impl Drop for Resume {
            fn drop(&mut self) {
                self.0.resume();
            }
        }
        let _resume = Resume(control.clone());
        ready(&mut h);
        assert_eq!(call(&mut h, 1, sensor_start())["ok"], true);
        running(&mut h);
        control.hang();
        std::thread::sleep(std::time::Duration::from_millis(220));
        let started = Instant::now();
        h.watchdog_tick();
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        wait_for(|| {
            h.watchdog_tick();
            h.plan_authorization.is_none()
        });
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Stopped);
        assert_eq!(h.authorization_end, Some("fault"));
        assert_eq!(h.plan_snapshot()["fans"][0]["state"], "automatic_confirmed");
        control.resume();
        std::thread::sleep(std::time::Duration::from_millis(120));
        h.watchdog_tick();
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Stopped);
    }
    #[test]
    fn production_does_not_admit_fake_or_uncalibrated_temperature_sources() {
        let mut h = HostState::new(false);
        assert_eq!(
            call(&mut h, 1, sensor_start())["error"]["code"],
            "backend_unavailable"
        );
        assert!(!h.sampler.supports("Tp01"));
        assert!(!h.sampler.is_alive());
        assert!(h.engine.statuses().is_empty());
    }
    #[test]
    fn existing_unverified_candidate_cannot_start_sensor_plan_or_apply_any_fan() {
        let mut h = HostState::new(true);
        h.sampler = sensors::Sampler::test_candidates(h.origin);
        let ids = ["smc:Tp01".to_owned()].into_iter().collect();
        wait_for(|| h.local_samples(&ids).is_some());
        assert_eq!(h.sensor_batch.as_ref().unwrap().readings.len(), 4);
        assert!(h
            .sensor_batch
            .as_ref()
            .unwrap()
            .readings
            .iter()
            .all(|r| r.sample.quality == macs_fan_control_runtime::policy::Quality::Unverified));
        let mut request = sensor_start();
        request["entries"][0]["strategy"]["sensor_id"] = json!("smc:Tp01");
        // Even an earlier fixed entry must not be applied by a rejected plan.
        request["entries"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"fan":1,"strategy":{"type":"fixed","rpm":3000}}));
        let reply = call(&mut h, 1, request);
        assert_eq!(reply["error"]["code"], "sensor_source_unavailable");
        assert!(h.engine.statuses().is_empty());
        assert!(h.plan_authorization.is_none());
        assert_eq!(reply["dry_run"], true);
        assert_eq!(reply["hardware_write_enabled"], false);
    }

    #[test]
    fn candidate_initialization_never_blocks_control_or_lifecycle_and_wake_stays_stopped() {
        let mut h = HostState::new(true);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        h.sampler = sensors::Sampler::test_candidate_initializing(h.origin, entered_tx, release_rx);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let before = Instant::now();
        h.watchdog_tick();
        let reply = call(&mut h, 1, start());
        assert_eq!(reply["ok"], true);
        h.lifecycle_begin(1);
        h.lifecycle_begin(2);
        assert!(before.elapsed() < std::time::Duration::from_millis(100));
        assert!(!h.sampler.is_alive());
        assert!(h.sensor_batch.is_none());
        assert!(h.plan_authorization.is_none());
        release_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(h.sampler.latest().is_none());
        h.watchdog_tick();
        assert!(!h.sampler.is_alive());
        assert!(h.plan_authorization.is_none());
    }

    #[test]
    fn sampler_panic_discards_cached_success_during_preparation() {
        let mut h = HostState::new(true);
        let (sampler, control) = sensors::Sampler::test_controlled(h.origin);
        h.sampler = sampler;
        ready(&mut h);
        assert_eq!(call(&mut h, 1, sensor_start())["ok"], true);
        control.panic();
        wait_for(|| !h.sampler.is_alive());
        h.watchdog_tick();
        assert!(h.plan_authorization.is_none());
        assert_eq!(h.engine.snapshot().phase, SessionPhase::Stopped);
        assert!(h.engine.statuses().is_empty());
        assert!(h.sensor_batch.is_none());
    }
    #[test]
    fn seven_execution_combinations_are_generated_and_denials_precede_rng_and_plan_checks() {
        let rows = [
            (
                HostExecution::ReadonlyPolicy,
                "smc_readonly",
                true,
                false,
                "readonly",
                "readonly_policy",
            ),
            (
                HostExecution::NoReviewedProfile,
                "smc_readonly",
                true,
                false,
                "readonly",
                "no_reviewed_profile",
            ),
            (
                HostExecution::ProfileValidationFailed,
                "smc_readonly",
                true,
                false,
                "readonly",
                "profile_validation_failed",
            ),
            (
                HostExecution::BackendOpenFailed,
                "unavailable",
                true,
                false,
                "readonly",
                "backend_open_failed",
            ),
            (
                HostExecution::BackendUnavailable,
                "unavailable",
                true,
                false,
                "readonly",
                "backend_unavailable",
            ),
            (
                HostExecution::Admitted,
                "smc_control",
                false,
                true,
                "profile_gated",
                "admitted",
            ),
            (
                HostExecution::TestOnly,
                "fake",
                true,
                false,
                "test_fake",
                "test_only",
            ),
        ];
        for (execution, backend, dry_run, write, kind, admission) in rows {
            // Compile-time wire fixture only. No SMC adapter is opened or admitted.
            let mut h = HostState::new(false);
            h.execution = execution;
            let expected = json!({"version":1,"execution":kind,"admission":admission});
            let status = call(&mut h, 1, json!({"op":"plan_status"}));
            assert_eq!(status["ok"], true);
            assert_eq!(status["backend"], backend);
            assert_eq!(status["dry_run"], dry_run);
            assert_eq!(status["hardware_write_enabled"], write);
            assert_eq!(status["control"], expected);
            if execution.permits_plan() {
                continue;
            }
            TOKEN_REQUESTS.with(|count| count.set(0));
            for command in [
                start(),
                json!({"op":"start_plan","entries":[],"ttl_ms":0,"seq":0}),
            ] {
                let denied = call(&mut h, 1, command);
                assert_eq!(denied["error"]["code"], admission);
                assert_eq!(denied["control"], expected);
                assert_eq!(denied["plan"]["phase"], "idle");
                assert_eq!(denied["plan"]["authorized"], false);
                assert!(denied.get("result").is_none());
                assert!(!denied.to_string().contains("token"));
                assert!(h.plan_authorization.is_none());
                assert!(h.issued_plan_tokens.is_empty());
                assert!(h.engine.statuses().is_empty());
            }
            TOKEN_REQUESTS.with(|count| assert_eq!(count.get(), 0));
            for op in ["renew_plan", "stop_plan"] {
                let mut command = json!({"op":op,"token":"00".repeat(32),"seq":1});
                if op == "renew_plan" {
                    command["ttl_ms"] = json!(5000);
                }
                let reply = call(&mut h, 1, command);
                assert_eq!(reply["error"]["code"], "no_active_plan");
                assert_eq!(reply["control"], expected);
            }
        }
    }
    #[test]
    fn success_business_failure_and_final_refresh_keep_constructor_fingerprint() {
        for execution in [HostExecution::TestOnly, HostExecution::Admitted] {
            // The admitted wire fixture delegates only to the existing Fake engine.
            // No production runtime switch or adapter profile injection exists.
            let mut h = HostState::new(true);
            h.execution = execution;
            let status = call(&mut h, 1, json!({"op":"plan_status"}));
            let expected = status["control"].clone();
            let started = call(&mut h, 1, start());
            assert_eq!(started["ok"], true);
            assert_eq!(started["control"], expected);
            let secret = started["result"]["token"].as_str().unwrap().to_owned();
            let mut forged = started.clone();
            forged["control"] = json!({"version":99,"execution":"forged","admission":"forged"});
            forged["backend"] = json!("forged");
            let refreshed = h.refresh_reply(forged, true);
            assert_eq!(refreshed["control"], expected);
            assert_eq!(refreshed["backend"], status["backend"]);
            for command in [
                start(),
                json!({"op":"renew_plan","token":"00".repeat(32),"ttl_ms":5000,"seq":2}),
                json!({"op":"stop_plan","token":"00".repeat(32),"seq":2}),
            ] {
                let failed = call(&mut h, 1, command);
                assert_eq!(failed["ok"], false);
                assert_eq!(failed["control"], expected);
                assert!(failed.get("plan").is_some());
                assert!(failed.get("result").is_none());
                assert!(!failed.to_string().contains(&secret));
            }
            let renewed = call(
                &mut h,
                1,
                json!({"op":"renew_plan","token":secret,"ttl_ms":5000,"seq":2}),
            );
            let stopped = call(&mut h, 1, json!({"op":"stop_plan","token":secret,"seq":3}));
            for reply in [renewed, stopped] {
                assert_eq!(reply["ok"], true);
                assert_eq!(reply["control"], expected);
                assert!(reply["result"].get("token").is_none());
            }
            h.execute(owner(1), Command::Tick(TickReason::Sleep));
            assert_eq!(
                call(&mut h, 1, json!({"op":"plan_status"}))["control"],
                expected
            );
        }
    }
}
