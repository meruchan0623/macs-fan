# Saved-preset desktop bridge (0.11)

The compact preset editor separates Save, Apply and Stop. Save only commits the
local preferences document. Apply sends a saved preset ID, the exact document
revision and a fresh cancellation request ID; it never sends RPMs, temperatures,
identity, capability claims or lease tokens. A dirty editor cannot apply. Before
admission the displayed draft and baseline must equal the saved object in the
current document. Clean editors reconcile concurrent changes; dirty editors keep
their draft, and Reset explicitly restores the latest saved object.

`src-tauri/src/control.rs` resolves that ID from the validated preferences store.
It checks the revision, live model/architecture, complete fan inventory, observed
RPM bounds and fresh snapshot. Sensor entries contain only the source ID and
configuration. The helper independently owns trusted observations and admission;
the desktop cannot grant write capability. The real helper currently rejects
sensor plans because no calibrated production sensor has been admitted.

Start and Stop each reserve a Rust-only increasing intent before asynchronous
preparation. A bounded request-ID ledger rejects reuse and remembers Stop even
when it arrives before the associated Start handler. At most one Start is pending.
Stop has an independent lane, marks a pending ticket cancelled and forwards its
higher intent to native. Native ordering closes the final Rust check-to-call race.
Sequence or ledger exhaustion prevents further starts but leaves emergency stop
available. Restart resets this in-memory ledger; it does not resume any plan.

The native client owns the persistent connection, private token and independent
renewal timer. See `helper/docs/control-client.md` for its closed ABI, authentication,
3-second monotonic deadlines, one-request backpressure, generation fencing and
recovery evidence. `control_status` takes a local snapshot and never renews consent.
The safe CLI `--control-status` exposes the same snapshot; no control-start CLI is
provided. Closing the window retains the process and connection; process exit or
connection death relies on host-side ownership revocation and the consent deadline.
This is not an assertion of an interruptible kernel I/O or independent hardware
watchdog.

The frontend has separate physical request slots for start, stop and status.
A 4-second presentation timeout does not release the actual request slot. Late
results cannot overwrite newer intent or the timeout; once the physical call
settles, a fresh status is requested. Active status requires fresh authenticated
readback, a live authorization and an actual writable backend. Stale or failed
recovery is shown as unconfirmed. The mock test backend cannot claim real control.

The current production Team is deliberately unconfigured; status is
`blocked/production_team_unconfigured`, Apply is disabled, and no connection is
opened. Even with identity configured, the current readonly helper handshake
refuses start. This version does not enable hardware control or register a service.

Verification is split between Rust intent/configuration tests, JS state tests,
real-browser mock-invoke interaction checks, native mock transport tests and a
real Rust-helper Fake schema check. None substitutes for signed production XPC,
root-service installation, calibrated sensors, verified hardware profiles or
actual fan recovery. The installed native-window check remains pending if the
Mac is locked.

In 0.12 a sensor plan can return authorized `preparing` while collecting two
independent local samples. The client retains its private token, renews consent
and permits Stop; it reports active only after the runtime reports running with
matching outcomes. The current actual backend remains readonly and unadmitted.
The explicit Fake host exercises this path without hardware access.

In 0.16 the host and native client use a closed execution/admission handshake;
see `helper/docs/production-control.md`. The actual production entry still selects
readonly and the hardware registry remains empty. UI receipt age uses the
monotonic performance clock. Invalid or regressing clock observations invalidate
old confirmation until a new response establishes a baseline. Fresh authenticated
automatic readback after Stop can remain confirmed after the writable connection
closes; active/preparing still require current write capability.
