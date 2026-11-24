use macs_fan_smc_adapter::SmcAdapter;
fn main() {
    // No parameters can request a write or supply capabilities/profile data.
    if std::env::args().len() != 1 {
        eprintln!("readonly-probe accepts no arguments");
        std::process::exit(2);
    }
    match SmcAdapter::open_readonly() {
        Ok(mut adapter) => match serde_json::to_string_pretty(&adapter.probe()) {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1)
            }
        },
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1)
        }
    }
}
