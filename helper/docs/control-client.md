# Desktop control client (0.16)

`src-tauri/native/ControlClient.m` owns one persistent NSXPC connection and its
plan authorization. The desktop Rust adapter calls these C functions from a
blocking worker, never the main UI thread:

```c
char *mf_control_start(const uint8_t *entries_json, size_t length, uint64_t intent_id);
char *mf_control_status(void);
char *mf_control_stop(uint64_t intent_id);
void mf_control_free(char *json);
```

Every non-null return is owned UTF-8 JSON and must be freed exactly once. A null
return means allocation/encoding failure. `entries_json` is a closed helper
entries array, 1–16 unique fan IDs, at most 16 KiB. Each entry has only `fan` and
`strategy`; strategy is `automatic`, `fixed` with `rpm`, or `sensor` with
`sensor_id`, `min_rpm`, `max_rpm`, `lower_temp`, and `upper_temp`. Native validates
types and bounds; helper remains authoritative for supported fans, targets,
sensors and hardware. The isolated Fake helper accepts only its local `test:temperature` source;
the deployed production entry remains read-only. The offline control constructor does not supply a reviewed production profile or sensor authorization.

Rust allocates a monotonically increasing nonzero `intent_id` before asynchronous
preset preparation. It never accepts this integer from frontend input. Start
and stop claim their intent on the native serial queue. Older intents are
rejected with `stale_intent` without changing the connection. A stop which arrives
before its earlier start therefore fences that start. `UINT64_MAX` is reserved
for the final stop; it is never accepted by start. This intent does not cross XPC.
The separate wire `seq` stays an exact native `uint64_t`, beginning at 1 and
incrementing for each renewal or stop. Exhaustion revokes the connection.

## Public JSON

All operations return schema version 1 with these fields:

| Field | Meaning |
| --- | --- |
| `operation` | `start_plan`, `control_status`, or `stop_plan` |
| `state` | `idle`, `blocked`, `preparing`, `active`, `automatic_confirmed`, `recovery_pending`, or `unknown` |
| `reason` | Fixed local reason or validated helper error enum |
| `hardware_write_enabled` | True only for a fresh, authenticated `smc_control/profile_gated/admitted` fingerprint on a still-live connection; independent of plan authorization or confirmed actuation. Fake stays false |
| `authenticated_response` | A structurally valid response was received through the configured connection; historical evidence, not current authorization |
| `current` | Last validated response is at most 1500 ms old and the connection survives, or an explicit stop just confirmed automatic recovery |
| `authorization_live` | Current response says authorized and a private token and live connection still exist |
| `last_response_age_ms` | Monotonic age of the last validated response, or null |
| `request_in_flight` | Exactly one wire request is awaiting completion |
| `can_attempt_start` | Local OS and production identity checks pass and no connection/request exists; this is not hardware write permission |
| `can_stop` | A connection or pending request can be cancelled |
| `plan` | Closed, validated helper plan snapshot, or null; never contains a token |

A plan snapshot contains the helper's phase, individual fan outcomes,
watchdog count, consent expiry, authorization end, error and stop reason. Its
helper clock timestamps are not desktop wall-clock times. Preserve `current`
and response age when displaying it: retained snapshots can describe historical
active targets after a disconnect. A start request alone never yields `active`.
The reply must have the expected plan kind, valid private token, authorization,
running phase, complete matching fan IDs, requested fixed targets or sensor
targets within requested RPM bounds, and confirmed fan outcomes. Unknown or recovering outcomes remain unconfirmed.

An authorized `preparing` reply retains the token and connection and renews
consent while awaiting independent helper-local temperature samples. It means
no active fan write has been confirmed. Every requested fan must be present and
may only be `unobserved` or previously `automatic_confirmed`; an active fan,
extra/missing ID, absent consent or ended authorization is rejected. The public
state is `preparing` even when all fans have an earlier automatic readback.
A later validated running reply may become `active`. Stale preparing evidence
becomes `unknown`; a timeout or lost connection becomes unconfirmed recovery.
Neither late replies nor local polling revive a disconnected authorization.
Preparing remains stoppable. The UI shows “准备中” only with fresh authenticated,
writable, authorized preparing evidence; Fake keeps hardware writes false and
cannot claim real control.

An explicit stop which confirms all fans automatic keeps that observation
current for 1500 ms after disconnect. Failure or interrupted stop reports
`recovery_pending`; disconnect initiates helper-side recovery but does not prove
that recovery succeeded. Local status polling does not perform XPC or renew
consent. It does perform the local identity preflight, so an unconfigured first
status is `blocked`, not an apparently ready idle state.

## Lifecycle and production boundary

The client requires macOS 13 before using the public code-signing requirement
selector. It compiles the fixed helper requirement, validates its own live
`SecCode` against the fixed client requirement, installs the helper requirement
before resume, and connects only to `MFProductionMachServiceName()` with the
privileged option. The build-time Team ID remains empty. No service registration,
credential discovery, root operation or SMC write is performed by this client.

After authentication, start first sends `plan_status` on the same connection.
Every parsed plan reply requires the closed `control={version,execution,admission}`
object, integer version 1, closed Plan, operation-specific success/error envelope,
and the exact backend/flag combination. Strict schema validation applies to
parsed fields, types and combinations using the existing `NSJSONSerialization`
semantics; this change adds no raw JSON duplicate-key detector. The host's
serde-generated responses have unique fields. This response boundary is separate
from serde's duplicate-field rejection on helper requests.
The seven combinations are frozen in
[`control-wire-contract-0.16.md`](../../../control-wire-contract-0.16.md): three
`smc_readonly` reasons (`readonly_policy`, `no_reviewed_profile`,
`profile_validation_failed`), two `unavailable` reasons (`backend_open_failed`,
`backend_unavailable`), `smc_control/profile_gated/admitted`, and isolated
`fake/test_fake/test_only`. Only the latter requires a compile-time harness plus
`Bridge.allow_fake`; a normal binary rejects Fake. Production start requires the
admitted control combination. A valid readonly/unavailable preflight remains an
authenticated observation, returns `blocked` with its exact admission reason,
disconnects, and sends no start. The deployed production entry still selects the
readonly constructor; its Team ID remains unconfigured.

The first validated preflight stores the exact backend, flags and control object
as a connection fingerprint. Each start, renewal, stop and business-error reply
must retain it. Any change, including another individually valid combination,
revokes the token and fingerprint, invalidates the connection generation, and
reports unconfirmed recovery. The changed plan cannot confirm a stop. Missing
control or generic protocol errors do not establish any capability. A completed
handshake proves neither applied targets nor successful automatic restoration.

The inspection client uses the same fixed service, but its ABI never authorizes
plans: its outer `hardware_write_enabled` and `production_control_ready` remain
false for that RPC. The nested authenticated response truthfully retains a
`smc_readonly/true/false` or `smc_control/false/true` host combination. Success
requires hardware reads; `unavailable/true/false` is accepted only as a helper
error. Readonly inspection has false profile verification and zero write counts;
control-host inspection may retain either boolean verification and nonzero
cumulative counts. Both counts must be exact nonnegative integers and agree.
Its reason is `authenticated_hardware_inspection`, describing this observation,
not a claim that the entire host lacks a write path. Inspection never supplies
the plan handshake, token or `authorization_live`.

An admitted start sends TTL 5000 ms and seq 1. A native dispatch timer
renews once per second independently of Rust/UI polling, with only one request
in flight. There is no retry or automatic restart. Each action/request has an
absolute monotonic 3-second deadline; start's preflight and start request share
the same budget. Reply handlers check the deadline both before parsing and at
state claim. Timer and response paths close on timeout, malformed response,
sequence exhaustion, exception or disconnection. Generation plus request ID
checks make old callbacks inert after cancellation or a new start.

If stop arrives during an in-flight start/renewal, it invalidates immediately,
resolves the pending caller and reports `stop_cancelled_inflight_request` with
recovery unconfirmed. It does not queue a second wire request behind an
unresponsive one. Otherwise it sends `stop_plan` with the same private token,
then invalidates on success or failure. Process exit/connection death and helper
sleep handling revoke consent on the host; the desktop never restarts a plan on
wake. The client retains the token only in native memory and never returns raw
helper responses, exceptions or arbitrary error strings.

## Verification

Run from the project workspace:

```sh
python3 outputs/macs-fan/helper/scripts/control_client_probe.py
```

The nonroot probe compiles the actual normal client with ProductionIdentity and
checks the empty-Team denial, lack of test entrypoints and lack of dynamic
availability runtime symbols. Isolated builds exercise identity ordering,
readonly preflight, renewal independent of status, exact integer sequence,
stop/start intent races, single in-flight backpressure, timeout, claim-time late
reply, generation isolation, disconnect, unconfirmed recovery, exception
containment, input validation and secret exclusion. A separate variant links
the real Rust helper with a Fake backend to check actual plan reply schema.
The accelerated race harness uses 200 ms deadlines; production stays 3 seconds.
Preparing renewal/stop, transition to running, invalid fan/write evidence,
sensor target bounds and late replies are tested with isolated mock transport.
The real helper schema check continues to use a fixed Fake plan. The added matrix checks all seven combinations on all four success and business-error operations, each single-field flip, missing/unknown fields, boolean versions, five authenticated preflight denials, and changed start/renew/stop fingerprints. An admitted compile-time stub demonstrates the write claim, stale connected evidence removes it, and confirmed stop/disconnection keep it false. Results and exact command/output logs are
saved under `work/control-native-0.16/`. `--stub-only` runs native isolation without building the Rust helper; omit it for Fake/helper schema integration.
These tests prove the client state machine and wire compatibility, not a real
production write channel, fan actuation, restoration or hardware admission.
