//! Explicit, bounded, read-only probe. No root/helper/service or writes.
use macs_fan_smc_adapter::temperature::TemperatureReader;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let keys: Vec<String> = std::env::args().skip(1).collect();
    if keys.is_empty() || keys.len() > 4 {
        return Err("supply 1..4 existing temperature keys".into());
    }
    let mut reader = TemperatureReader::open()?;
    let readings: Vec<_> = keys
        .iter()
        .map(|key| serde_json::json!({"key": key, "result": reader.read(key)}))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "assessment": "uncalibrated temperature candidates; not control verified",
            "operation": "read_only", "readings": readings
        }))?
    );
    Ok(())
}
