use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let proto_root = manifest_dir.join("../../..");
    let omega_dir = proto_root.join("omega");
    let generated_dir = manifest_dir.join("src/generated");
    let generated_file = generated_dir.join("hyperion.omega.v1.rs");

    let proto_files = [
        omega_dir.join("consensus.proto"),
        omega_dir.join("zk_solvency.proto"),
        omega_dir.join("margin_risk.proto"),
        omega_dir.join("wasm_quant.proto"),
        omega_dir.join("settlement_l2.proto"),
    ];

    for pf in &proto_files {
        println!("cargo:rerun-if-changed={}", pf.display());
    }
    println!("cargo:rerun-if-env-changed=HYPERION_OMEGA_REGEN_PROTO");

    let force_regen = env::var("HYPERION_OMEGA_REGEN_PROTO")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if force_regen || !generated_file.exists() {
        std::fs::create_dir_all(&generated_dir)?;
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .extern_path(".hyperion.v1", "::hyperion_proto::v1")
            .out_dir(&generated_dir)
            .compile_protos(&proto_files, &[proto_root])?;
    }

    Ok(())
}
