fn main() {
    // Compile the privileged wake helper (IOKit wake scheduler) as a resource.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let source = format!("{}/wake-helper.mm", manifest);
        let out_dir = format!("{}/bin", manifest);
        let output = format!("{}/meternest-wake-helper", out_dir);
        let _ = std::fs::create_dir_all(&out_dir);
        let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64".to_string(),
            Ok(other) => other.to_string(),
            Err(_) => "arm64".to_string(),
        };
        let status = std::process::Command::new("clang++")
            .args([
                "-arch",
                arch.as_str(),
                "-std=c++17",
                "-fobjc-arc",
                "-framework",
                "Foundation",
                "-framework",
                "IOKit",
                "-o",
                &output,
                &source,
            ])
            .status();
        match status {
            Ok(code) if code.success() => println!("cargo:rerun-if-changed={}", source),
            other => {
                // A missing helper only disables the optional wake feature.
                println!("cargo:warning=wake-helper compile failed: {:?}", other);
                println!("cargo:rerun-if-changed={}", source);
            }
        }
    }
    tauri_build::build();
}
