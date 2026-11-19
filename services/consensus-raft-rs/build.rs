use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);

    // Canonical monorepo location `../../proto`, with automatic fallback to `../proto_mirror` during isolated staging.
    let monorepo_proto = manifest_dir.join("../../proto");
    let staging_proto = manifest_dir.join("../proto_mirror");
    let proto_root = if monorepo_proto.join("omega/consensus.proto").exists() {
        monorepo_proto
    } else {
        staging_proto
    };

    let common_proto = proto_root.join("common.proto");
    let orderbook_proto = proto_root.join("orderbook.proto");
    let consensus_proto = proto_root.join("omega/consensus.proto");

    println!("cargo:rerun-if-changed={}", common_proto.display());
    println!("cargo:rerun-if-changed={}", orderbook_proto.display());
    println!("cargo:rerun-if-changed={}", consensus_proto.display());

    // Ensure `protoc` from `~/.cargo/bin` is discoverable on Windows even if PATH was minimal.
    if env::var_os("PROTOC").is_none() {
        if let Ok(user_profile) = env::var("USERPROFILE") {
            let candidate = PathBuf::from(user_profile).join(".cargo/bin/protoc.exe");
            if candidate.exists() {
                env::set_var("PROTOC", candidate);
            }
        }
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[common_proto, orderbook_proto, consensus_proto],
            &[proto_root],
        )?;

    Ok(())
}
