# Read-only temperature reader

```rust
use macs_fan_smc_adapter::temperature::{TemperatureReader, TemperatureReading};
// TemperatureReader::open() -> Result<TemperatureReader, AdapterError>
// TemperatureReader::read(&mut self, key: &str) -> Result<TemperatureReading, AdapterError>
let mut reader = TemperatureReader::open()?;
let candidate = reader.read("Tp01")?;
```

`TemperatureReading` carries hardware `identity`, exact `key`, `metadata` (type, size,
attributes), `candidate_celsius`, and `suspect`. Every successful reading is an
uncalibrated candidate. `suspect == false` does not mean physically verified,
calibrated, suitable for control, or correctly mapped to a component. Zero is
retained with `suspect == true`; there is no correction, imputation, catalogue,
aggregation, or fallback. Negative candidates within the stated range are retained.

Keys must be exactly four ASCII graphic bytes beginning with `T`; spaces, Unicode,
control bytes and fan keys are refused before device I/O. Only `flt ` with size 4
(little-endian IEEE-754) and `sp78` with size 2 (big-endian signed 7.8 fixed point)
are accepted. Nonfinite values, wrong payload lengths, and values outside the inclusive
-40..150 °C candidate range produce `AdapterError::Malformed`, never a clamped value.
Missing keys, IOKit/SMC codes, timeouts, identity changes and metadata changes remain
explicit errors; a failed read never returns an earlier successful value.

The reader owns a fresh AppleSMC connection, independent of `SmcAdapter` and its
actuator locks. The internal read-only transport has only identity/info/read methods;
it emits SMC commands 9 and 5. It reuses the existing checked 80-byte ABI call and
connection cleanup without expanding the actuator `Key` enum or the private profile
`Admission` rules. No write API or promotion API is exposed. Each read checks identity
before and after, fetches metadata before and after bytes, and rejects any drift.
There is no key-info or value cache. These checks detect observed drift; a synchronous
kernel call is not an atomic snapshot. The existing 250 ms late-response refusal is
preserved; a blocked kernel call cannot be cancelled without process isolation.

## Bounded hardware probe

Run from the project root without root, service registration, or helper installation:

```sh
cargo run --manifest-path smc-adapter/Cargo.toml --example temperature-probe -- Tp01 TCMb TB0T TAOL
```

The example requires 1..4 explicit keys and only reads them. On 2026-11-22 the keys
were chosen from `../evidence/backend-snapshot-0.11.json`, not discovered by a scan.
The live Mac16,8/aarch64 probe returned `flt `/4, attributes 133 for all four:
Tp01 40.0 °C, TCMb 41.07195281982422 °C, TB0T 23.79998779296875 °C,
TAOL 23.25 °C. These observations establish only successful current protocol reads,
not sensor calibration or readiness to control fans. No actuator state was changed.

Evidence lives in `work/temperature-reader-0.12/`: `probe.log`, `test.log`,
`clippy.log`, and `commands.txt`. Adapter tests passed (23 total, including five
independent temperature tests). `cargo clippy --all-targets -- -D warnings` passed.
The fake I/O surface cannot represent writes and records the exact identity/info/read
sequence, including a successful zero followed by a timeout with no cached fallback.
Tests cover key validation, exact sizes, endianness, signed fixed-point, NaN/Inf,
range boundaries, missing keys, metadata drift, and identity drift before/after read.
