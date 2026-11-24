# Live target adjustment

`Command::SetTarget { fan, token, rpm, ttl_ms, seq }` adjusts an authenticated,
unexpired lease. It retains the token and requires the same owner and a strictly
increasing sequence. Invalid input leaves a healthy lease unchanged. Expiration
is processed before dispatch and cannot be undone by a target adjustment.

`Capabilities::quantize_target` validates the capabilities and requested range,
rounds to the nearest hardware step (`round`, with positive ties upward), then
requires the result to be in range and exactly representable by the encoding.
`Acquire` continues to require an already representable RPM; a sensor policy can
use this helper before acquiring its initial target.

Before writing, adjustment revalidates current capabilities and reads back the
previously confirmed manual target. A quantized alias renews the deadline and
sequence after this check without writing. A changed target performs one target
write and verifies both manual mode and the new target. Only then are the
confirmed target, sequence and checked absolute deadline committed.

An in-flight update retains the previous confirmed target and separately tracks
the attempted target. On write/readback failure the token is revoked. Recovery
can restore automatic only after reading manual mode with either the old or the
attempted target. A different finite manual target or externally observed
automatic mode yields control. Unknown state or a failed ownership read consumes
the bounded retry budget without a mode write. Recovery attempts never allocate
a replacement lease or reset the retry budget.

Validation is hardware independent: ten update tests exercise successful
adjustment, quantized aliases, invalid identity/input, expiration and overflow,
capability changes, write errors before and after effect, ignored writes,
read errors, external mode/target takeover, bounded failed restoration and
ownership reads that fail. The existing 41 transaction tests remain unchanged.
