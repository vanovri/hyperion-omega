use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir.join("../../../proto");
    let generated_dir = manifest_dir.join("src/generated");
    let generated_file = generated_dir.join("hyperion.v1.rs");

    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("common.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("orderbook.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("telemetry_fraud.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("chronos.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("stream_analytics.proto").display()
    );
    println!("cargo:rerun-if-env-changed=HYPERION_REGEN_PROTO");

    let force_regen = env::var("HYPERION_REGEN_PROTO")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if force_regen || !generated_file.exists() {
        std::fs::create_dir_all(&generated_dir)?;
        let protos = [
            proto_dir.join("common.proto"),
            proto_dir.join("orderbook.proto"),
            proto_dir.join("telemetry_fraud.proto"),
            proto_dir.join("chronos.proto"),
            proto_dir.join("stream_analytics.proto"),
        ];
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .out_dir(&generated_dir)
            .compile_protos(&protos, &[proto_dir])?;
    }

    Ok(())
}
