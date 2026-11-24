# Trusted policy coordinator

This crate executes a host-local multi-fan plan through `macs-fan-control-core::Engine`. It supplies the sustained software loop for fixed targets and sensor curves, with explicit session start, finite leases, periodic renewal, target changes, and whole-session rollback. It contains no hardware adapter, OS API, IPC decoder, or unsafe code. The shipped desktop app remains read-only; these tests are software evidence against `FakeActuator`, not production control or thermal-safety validation.

`src/policy.rs` is the shared original policy implementation. Its recommendations remain research math. Only the coordinator converts a validated recommendation into capability-checked engine transactions.

## Host contract

Construct `Coordinator::new(actuator, tokens, clock)` with a dedicated actuator, a trusted token source, and a trusted monotonic `Clock`. The constructor creates a fresh private engine ledger; it cannot accept an existing engine with invisible active leases or recovery work. Existing engines must remain with their current owner until their leases and recovery are resolved. Construction performs no hardware read or write, so `Idle` describes the session only and does not assert automatic physical mode. Sensor timestamps and that clock must share an epoch. The host supplies an `Owner` only after transport authentication and feeds verified samples from its own sampler. A `SensorId` is a stable sampler identifier, not a display label. These types deliberately have no deserialization or wire API. They do not authenticate caller-created Rust values by themselves.

Direct commands and plans share this coordinator's one private engine. The host must not create a second engine for the same controlled hardware. `statuses() -> Vec<FanStatus>` and `status(FanId) -> Option<FanStatus>` expose the complete ledger, including direct leases and recovery, without I/O; neither exposes mutable engine access. `snapshot()` describes the plan session only, so an `Idle` plan snapshot can coexist with direct leases visible in `statuses()`.

`execute_direct(owner, command) -> Result<Response, Error>` accepts authenticated `Enumerate`, `Capabilities`, `Acquire`, `Renew`, `SetTarget`, and `Release`, using the same trusted clock, post-I/O deadline checks, owner authorization, and engine recovery as plans. It rejects every `Command::Tick` with `Error::InvalidDirectCommand`; trusted lifecycle events use the separate methods below. Read-only queries remain available during plans. Direct mutations return `Busy` while a plan is `Preparing` or `Running`, or while any attempted fan in a stopped plan lacks `AutomaticConfirmed` evidence. The first admitted direct mutation retires a safely stopped historical session to `Idle`, preserving the entire engine ledger. This permits direct renewal/release on former plan fans without treating those new leases as unresolved old plan attempts. A failed command cannot discard an active or unconfirmed plan.

Call `start(owner, Plan, samples)` only after an explicit user action. Loading saved preferences must never invoke it. `Plan` has 1–16 unique fans, `ttl_ms` in 1–5000, and each fan uses `Automatic`, exact `Fixed { rpm }`, or `Sensor { sensor, config }`.

Call `step(samples)` on every sampling cycle, including an empty slice after failed reads. Supply every referenced sensor once; the same sensor can drive several fans. Never relabel cached samples with a new timestamp. The runtime retains bounded replay history across stopped/replaced plans (up to 256 sensor IDs per coordinator lifetime).

Independently schedule `watchdog()` even when UI requests or the sampling task stop. It invokes core `Tick(Periodic)` to enforce expiration, audit ownership/readback, and retry recovery even when no plan session exists and only direct leases are present. After that I/O, a fresh clock read validates the stored sensor timestamp against its configured sample timeout, including while `Preparing`; an expired sample stops the entire session even if no `step` call ever arrives. A sample is valid at the timeout boundary and expires one millisecond later. `step` also invokes it, but that is not a replacement for an independent host timer. The host must serialize all access to the coordinator and bound each actuator operation so a blocked hardware call cannot indefinitely block the watchdog. Rust `&mut self` prevents concurrent transactions, not operating-system stalls.

The injected clock is read before and after engine calls. If I/O consumes a lease, the call fails and drives revocation instead of publishing an expired grant. Sensor freshness is rechecked between fan transactions and before returning `Running`. `snapshot()` is a view of the latest engine evidence; it does not perform I/O or promise that physical state has not changed since that evidence.

Call `stop(Explicit)` for plan stop, `disconnect(owner)` for a disconnected authenticated connection, and `stop(Sleep)` or `stop(Shutdown)` from trusted lifecycle paths. Explicit stop affects the current plan; with no live plan it does not revoke direct leases. `disconnect(owner) -> Result<Snapshot, Error>` always delivers the core owner-scoped disconnection event, including with no session; it stops a plan only when that same owner owns it. Disconnecting connection A cannot stop connection B's plan. The legacy `stop(Disconnected)` means disconnect the current live plan's owner and should not replace `disconnect(owner)` in transport cleanup. Sleep and shutdown revoke every owner's leases and suspend the engine even with no plan. Clock rollback also reaches the complete core ledger, revokes every owner, and marks its clock untrusted, including when the runtime's last post-I/O clock read was later than the engine's last command timestamp. Sleep, shutdown, and clock rollback make this coordinator terminal. Rebuild after wake/restart with fresh hardware validation and new explicit consent. Never discard a coordinator with pending/exhausted recovery to bypass that state. Process death still requires an independent privileged host/watchdog; this library cannot recover after its process ceases executing.

## Execution and outcomes

Before the first acquisition, startup checks inventory, unique/bounded fan IDs, verified capabilities, fixed RPM representation, sensor configuration and range, sample quality/freshness, and deadlines for the entire plan. `recovery_samples > 1` leaves the session `Preparing` until all sensor policies are ready, with no fixed or sensor fan writes during preparation. Every subsequent sampling step computes all targets before issuing any transaction.

Fixed RPMs must be exactly representable. Sensor endpoints must be representable and inside hardware bounds; continuous intermediate values use the core's `Capabilities::quantize_target` (nearest supported step). Identical targets use `Renew`; changed targets use `SetTarget` with the same private token and increasing sequence. Only an engine readback-confirmed lease grant is stored as active. Targets are commanded RPM, not measured physical RPM.

Multi-fan changes are sequential, not hardware-atomic. Any acquisition/update failure or invalid/missing/stale/replayed/future sample stops the whole session and revokes all active leases for that dedicated authenticated owner. Earlier fan writes may have occurred; rollback is reported separately and is never asserted successful merely because it was requested. A good subsequent sample cannot automatically restart a stopped session.

| Fan outcome | Meaning |
| --- | --- |
| `Active` | Latest core evidence confirmed manual mode and commanded target; finite deadline included |
| `AutomaticConfirmed` | Core actually read back automatic mode |
| `RecoveryPending` | Automatic recovery failed; core still has a bounded retry scheduled |
| `RecoveryExhausted` | Core exhausted its retry budget; no successful restoration claimed |
| `Yielded` | External control was observed; the runtime does not fight it |
| `Unknown` | A transaction was attempted without an authoritative resulting phase |
| `Unobserved` | This fan has no engine state evidence; never means automatic was confirmed |

An `Automatic` plan entry is observe-only. It never acquires a lease just to force an unowned manual fan automatic. With no prior engine evidence it remains `Unobserved`, including on stop. This API therefore cannot assert that an entire preset is physically automatic simply because those entries were requested. To switch a runtime-owned fan automatic, explicitly stop its current session and require confirmed restoration before starting the replacement plan.

A new session is allowed only after the prior session is stopped, every attempted fan is `AutomaticConfirmed`, and every existing engine ledger entry is `AutomaticConfirmed`. Any direct `Active`, `Recovering`, or `Yielded` entry blocks plan start, even on another fan. Pending, exhausted, yielded, and unknown outcomes prevent replacement in this coordinator. Its owner identity must be dedicated to the session: whole-session rollback uses the core's authenticated-owner revocation event.

## Verification

Run from the repository directory:

```sh
cargo test --manifest-path outputs/macs-fan/control-runtime/Cargo.toml
cargo clippy --manifest-path outputs/macs-fan/control-runtime/Cargo.toml --all-targets -- -D warnings
```

The suite covers the unchanged seven policy tests, the constructor compile-fail doctest, and 39 coordinator tests for complete mixed presets, startup sample recovery, rapid increase/held and rate-limited descent, quantization, preflight without writes, sensor fault classes, partial multi-fan rollback, external ownership, lease expiry and deadline overflow, clock rollback, recovery retry/exhaustion, lifecycle revocation, replay across restarts/plan changes, and slow I/O consuming sensor freshness or lease lifetime. All actuator writes in these tests affect in-memory fake state only.

Additional single-ledger cases cover direct/plan exclusion for every mutating command during preparation and execution, complete status visibility, retirement of confirmed stopped plans, rejection of client-supplied ticks, direct expiry/recovery without a session, owner-scoped disconnect, terminal events with multiple direct owners, direct acquisition/update/renewal blocked by slow I/O consuming the grant, and clock rollback both during I/O and between core/runtime clock watermarks. The complete crate suite contains 47 tests; the original 32 continue to pass.

Post-I/O recovery audits every expired ledger entry. Expiry of an unrelated direct lease does not discard a successful still-live grant for another owner; its token is returned so the caller can renew or release. A returned lease is rechecked after any expiry audit I/O. Expiry of any attempted fan in a live plan still fails the whole plan transaction.
