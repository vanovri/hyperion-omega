use std::path::{Path, PathBuf};

fn locate_proto_dirs(manifest_dir: &Path) -> (PathBuf, PathBuf) {
    // 1. Canonical monorepo relative path when located at `<repo>/services/quant-wasm-host/host`.
    let repo_proto = manifest_dir.join("../../../proto");
    let repo_omega = repo_proto.join("omega");
    if repo_proto.join("common.proto").exists() && repo_omega.join("wasm_quant.proto").exists() {
        return (repo_proto, repo_omega);
    }

    // 2. Local bundled proto fallback inside `<service>/proto_local` (used only during isolated pre-worktree staging).
    let local_proto = manifest_dir.join("../proto_local");
    let local_omega = local_proto.join("omega");
    if local_proto.join("common.proto").exists() && local_omega.join("wasm_quant.proto").exists() {
        return (local_proto, local_omega);
    }

    panic!(
        "Unable to locate common.proto / orderbook.proto / omega/wasm_quant.proto from {}",
        manifest_dir.display()
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let (proto_dir, omega_dir) = locate_proto_dirs(&manifest_dir);

    let common_proto = proto_dir.join("common.proto");
    let orderbook_proto = proto_dir.join("orderbook.proto");
    let wasm_quant_proto = omega_dir.join("wasm_quant.proto");

    println!("cargo:rerun-if-changed={}", common_proto.display());
    println!("cargo:rerun-if-changed={}", orderbook_proto.display());
    println!("cargo:rerun-if-changed={}", wasm_quant_proto.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[common_proto, orderbook_proto, wasm_quant_proto],
            &[proto_dir, omega_dir],
        )?;

    Ok(())
}
