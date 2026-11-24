//! Standalone read-only probe. Build with rustc; no Cargo/Tauri dependencies.
#[path = "../src-tauri/src/hid.rs"]
mod hid;

fn quoted(value: &str) -> String {
    let mut result = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character if character < '\u{20}' => {
                result.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => result.push(character),
        }
    }
    result.push('"');
    result
}
fn optional(value: &Option<String>) -> String {
    value.as_deref().map_or_else(|| "null".into(), quoted)
}
fn main() {
    let batch = hid::sample();
    println!("{{\n  \"available\": {},\n  \"capturedAtMs\": {},\n  \"serviceCount\": {},\n  \"truncated\": {},\n  \"readings\": [",batch.available,batch.captured_at_ms,batch.service_count,batch.truncated);
    for (index, reading) in batch.readings.iter().enumerate() {
        println!("    {{\"sourceId\":{},\"registryId\":{},\"product\":{},\"temperatureC\":{},\"quality\":{},\"message\":{}}}{}",
            quoted(&reading.source_id),reading.registry_id,optional(&reading.product),
            reading.temperature_c.map_or_else(||"null".into(),|value|value.to_string()),
            quoted(reading.quality.as_str()),optional(&reading.message),if index+1==batch.readings.len(){""}else{","});
    }
    println!(
        "  ],\n  \"diagnostics\": [{}]\n}}",
        batch
            .diagnostics
            .iter()
            .map(|message| quoted(message))
            .collect::<Vec<_>>()
            .join(",")
    );
}
