# Local temperature sampling

The helper owns sensor identities, quality admission, and sampling time. None is
accepted from a wire request. `Sampler::fake(origin)` provides exactly
`test:temperature`, fixed at 60 °C, for the explicit fake host. Its independent
worker samples immediately and then waits 100 ms between completed reads.

The normal unavailable and `smc_readonly` hosts use `Sampler::unavailable()`.
There is no production temperature calibration/admission table. Hardware sensor
catalogs and raw SMC values therefore cannot become `Verified` readings merely
because they exist, parse, or resemble the fake source. This work adds no SMC
write, helper registration, installation, or signing operation.
Any separate real temperature reader remains an unadmitted candidate; adding
read-only decoding does not authorize its readings for a sensor-driven plan.

## Worker and host contract

The worker exclusively owns its reader, including any potentially blocking I/O.
There is one worker per sampler and one atomic slot for the latest completed
batch. Publishing replaces an older unconsumed successful batch. There is no
request queue, pending task backlog, reader mutex, or worker restart. Source IDs
are compared exactly, deduplicated, and limited to 16. Duplicate declarations
cannot promote a source's quality admission.

`latest(&mut self)` exchanges the slot with null and transfers ownership of a
complete batch. It has no sampling mutex or wait, and returns each publication
at most once. On the supported macOS targets the pointer atomic exchange is a
native atomic operation. Batch disposal and host caching still use ordinary
bounded Rust allocation/deallocation; this is not a hard real-time guarantee.
The atomic pointer is never dereferenced while stored in the slot. Publisher
and consumer exclusively own only pointers obtained by an exchange; the last
shared mailbox owner disposes of any remaining batch.

Every batch has a monotonically increasing sequence, and all its readings share
the time immediately before its reader call starts. That time is milliseconds
elapsed from the **same `Instant` origin as the host runtime**. A delayed read
keeps its original time and can already be stale when completed. Polling and
caching never refresh it. If two attempts would begin in the same millisecond,
the worker waits for actual clock advancement; it does not invent `last + 1`.

Reader errors, incomplete batches, non-finite values, and temperatures outside
the policy's -20…127 °C domain yield `Failed` for every configured source
and terminate that worker. This preserves an unconsumed failure rather than
letting a later success hide it. Unwinding also clears the worker's alive flag.
`latest()` discards successful batches from a terminated worker but can return
its terminal failed batch. The host must check `is_alive()` when considering
its own cached reading and stop an existing sensor plan if the worker died.
There is no automatic plan recovery or replacement worker.

The host may retain the last batch for an explicit start request, subject to
runtime freshness validation. When no initial completed batch is available,
start fails with `sensor_source_unavailable`; a later explicit retry can succeed.
Timer ticks only submit a newly consumed sequence to policy execution. Ticks
without a new batch still run the watchdog, allowing stale or hung sampling to
fail closed without waiting for I/O. Failed/dead sampling must invalidate cached
success. Host lifecycle authorization remains independent of this sampler.

Dropping a sampler requests cancellation and unparks a sleeping worker, then
detaches its thread handle. It never joins. If a reader is hung, that one thread
and its bounded state remain until I/O returns or the process exits; no new
worker is created. A read returning after cancellation cannot publish a batch.

## Verification

`sensors.rs` contains isolated tests for exact IDs and the shared epoch,
unavailable production sources, one-time consumption, delayed timestamps,
non-waiting polling and drop, terminal failure and panic, bounded conservative
source admission, a single overwritten slot, concurrent atomic ownership
transfer, and waiting for real millisecond advancement.

Run the sampler tests from the source project with:

```sh
cargo test --manifest-path helper/Cargo.toml sensors::tests
```

`cfg(test)` additionally exposes `test_controlled(origin)` and a control handle
with `fail()`, `hang()`, `resume()`, and `panic()` for host integration tests.
These controls affect the next worker read and do not add a wire command or
change the fixed-value normal fake provider.
