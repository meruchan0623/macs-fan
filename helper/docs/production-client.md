# Production observation client

`src-tauri/native/HelperClient.m` supplies the GUI-to-helper observation path. It sends exactly one fixed request and never registers a service, requests a lease, or calls a hardware write operation:

```json
{"version":1,"command":{"op":"inspect_hardware"}}
```

The C boundary is `char *mf_helper_inspect(void)` plus `mf_helper_client_free(char *)`. A non-NULL return is owned UTF-8 JSON and must be freed once. There are no caller-controlled service names, identities, signing requirements, commands, environment overrides, or plist policy inputs in the production connector.

The connector checks macOS 13 before identity work or connection construction, obtains the shared source-fixed `ProductionIdentity` requirements, checks the actual running executable with `SecCodeCopySelf` / `SecCodeCheckValidity(kSecCSStrictValidate)`, and compiles the helper requirement with Security.framework. Production currently stops at `production_team_unconfigured`; an ad-hoc binary also fails an Apple-anchored fake Team requirement in the probe. The connector does not change the configured Team, helper identifiers, service registration, or enabled state.

A configured build constructs `NSXPCConnection` for the fixed `local.macsfan.helper.control` name using `NSXPCConnectionPrivileged`. It sets `setCodeSigningRequirement:` before `resume`, exports only the matching `request:reply:` remote interface, and installs error, interruption and invalidation handlers before starting the connection. A valid NSXPC reply received under that requirement supplies channel authentication evidence. Service enabled state and successful object construction supply no such evidence.

The reply deadline is three monotonic seconds, starting before connection construction and request submission. Every terminal callback checks that absolute deadline while claiming the completion lock, so a reply arriving after the deadline cannot win while the waiting thread is paused. Waiting uses the remaining budget, and callbacks use a retained completion object rather than caller stack storage. The first terminal event wins; duplicate and late callbacks cannot replace it. The connector invalidates every constructed connection in `@finally`, including exceptional paths. Native builds use `-fobjc-arc-exceptions` so strong proxy/connection locals are released when an Objective-C exception unwinds; the probe checks zero live mock connections after terminal paths, including a valid reply followed by a thrown send exception. A process-wide `pthread_mutex_trylock` returns `helper_client_busy` immediately for overlapping or reentrant calls. No main-thread runloop is required.

This is a response acceptance/waiting deadline, not a hard cancellation guarantee for Foundation, Security, or helper hardware I/O. The app calls this synchronous C boundary from its blocking worker task.

Every wrapper has these fields:

```json
{
  "schema_version": 1,
  "operation": "inspect_hardware",
  "hardware_write_enabled": false,
  "production_control_ready": false,
  "authenticated_response": false,
  "result": "blocked",
  "reason": "production_team_unconfigured"
}
```

| Result | Meaning |
| --- | --- |
| `observed` | Authenticated `ok:true` read-only hardware inspection. `response` contains the parsed helper reply. |
| `helper_error` | Authenticated, valid `ok:false` helper error. This proves receipt, not hardware readiness. |
| `blocked` | OS/identity precondition or concurrent-call denial. No authenticated response. |
| `unavailable` | Transport failure, timeout, invalid reply, or contained native exception. No authenticated response. |

Success requires real JSON booleans, version 1, `dry_run:true`, `hardware_write_enabled:false`, `backend:"smc_readonly"`, `hardware_reads:true`, `result.kind:"hardware_inspection"`, an inspection object with `hardwareProfileVerified:false` and `hardwareWriteAttempts:0`, and a top-level zero write-attempt observation. Unknown top-level envelope fields, wrong result kinds, conflicting error/result fields, write claims/counts, profile claims, malformed JSON and payloads larger than 1 MiB are rejected. Error replies require a nonempty `error.code` of at most 128 characters and no result. These checks do not convert capability metadata into permission to control hardware. `production_control_ready` and `hardware_write_enabled` remain false even for a valid observation.

Run from the repository root:

```sh
python3 helper/scripts/production_client_probe.py
```

The script runs as the ordinary user and writes `helper/docs/production-client-verification.json`, including source and signed-binary hashes. It compiles the production connector with `-fobjc-arc-exceptions -Wall -Wextra -Werror -mmacosx-version-min=11.0` and verifies that test bridge and compiler availability-runtime symbols are absent. It never registers/unregisters a daemon, invokes `launchctl`, accesses SMC, or writes hardware.

The evidence separates three kinds of checks:

- Production source plus real `ProductionIdentity` returns the unconfigured-Team denial. An ad-hoc signed harness with a fake Apple Team requirement is rejected by real Security.framework.
- Compile-only connection mocks cover macOS gating before identity/new API, fixed privileged name and signing-order checks, schema rejection, background callbacks, timeouts and late callbacks, errors/interruption/invalidation, first terminal wins, exception containment/recovery, busy/reentrant handling and missing-factory denial without production fallthrough. Mock builds use a fixed 30 ms deadline. A deterministic race fixture includes the actual connector source and pauses the waiter for 80 ms after its 30 ms wait expires while a reply arrives at 35 ms; the result remains timeout.
- A real temporary embedded NSXPC service returns the fixed `fixture_readonly_no_hardware` error through the new connector. The client uses real Security self validation with a source-fixed, identifier-only test requirement and an exact helper CDHash compiled into its fixture binary. The temporary fixture server admits one of two predefined client identifiers through its fixed temporary admission-file path. A matching exchange returns authenticated `helper_error`; a wrong compiled helper CDHash and a genuinely differently signed client each fail without an authenticated response. The probe then changes only that fixture admission file to allow the wrong identifier and runs the identical wrong client bytes again: it receives authenticated `helper_error`. Both client and helper SHA-256 remain identical across this counterfactual; the helper CDHash compiled into the client is unchanged. That mutable admission policy exists only in generated test-server code, never the production connector. This test uses the normal three-second budget and a compile-only nonprivileged service factory. It proves protocol interoperability and real NSXPC peer requirement enforcement for this local fixture, not a configured production Team/channel or hardware inspection.

No test bridge or timeout flag can select a real production connection when compiled with `MF_HELPER_CLIENT_TEST_HARNESS`: an absent factory fails closed. These flags and bridge exports are absent from the production build.

SDK evidence was read from the installed Foundation `NSXPCConnection.h`: `NSXPCConnectionPrivileged` is the documented privileged-bootstrap option; constructing a connection does not establish service validity or launch success. `setCodeSigningRequirement:` is public macOS 13 API; malformed requirements throw, mismatching new messages invalidate the connection, and the SDK recommends setting it before `resume`. The macOS runtime gate precedes identity/connection work; a `respondsToSelector:` check then precedes typed `objc_msgSend` for this fixed public selector. This avoids a compiler availability-runtime dependency when Rust links with `-nodefaultlibs`, while failing closed if the selector is absent. No audit-token KVC, PID-derived authentication, private API, or original vendor helper is involved.
