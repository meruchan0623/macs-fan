fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for source in ["native/DiskTemperature.c", "native/DiskTemperature.h"] {
            println!("cargo:rerun-if-changed={source}");
        }
        cc::Build::new()
            .file("native/DiskTemperature.c")
            .flag("-std=c11")
            .flag("-mmacosx-version-min=11.0")
            .warnings(true)
            .warnings_into_errors(true)
            .compile("macs_fan_disk_reader");
        println!("cargo:rustc-link-lib=framework=IOKit");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        for source in [
            "native/ServiceManager.m",
            "native/ServiceManager.h",
            "native/HelperClient.m",
            "native/HelperClient.h",
            "native/ControlClient.m",
            "native/ControlClient.h",
            "../helper/native/ProductionIdentity.m",
            "../helper/native/ProductionIdentity.h",
        ] {
            println!("cargo:rerun-if-changed={source}");
        }
        cc::Build::new()
            .file("native/ServiceManager.m")
            .file("native/HelperClient.m")
            .file("native/ControlClient.m")
            .file("../helper/native/ProductionIdentity.m")
            .include("../helper/native")
            .flag("-fobjc-arc")
            .flag("-fobjc-arc-exceptions")
            .flag("-fblocks")
            .flag("-mmacosx-version-min=11.0")
            .warnings(true)
            .compile("macs_fan_service_manager");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=Security");
    }
    tauri_build::build()
}
