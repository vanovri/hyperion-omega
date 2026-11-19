use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=proto/netguard_control.proto");
    println!("cargo:rerun-if-changed=../../ebpf-c/hyperion_xdp_tc.bpf.c");
    println!("cargo:rerun-if-changed=../../ebpf-c/hyperion_bpf_common.h");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/netguard_control.proto"], &["proto"])
        .expect("failed to compile netguard_control.proto");

    // Opportunistically compile C eBPF object if `clang` with `-target bpf` is installed on PATH.
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let c_src = manifest_dir.join("../../ebpf-c/hyperion_xdp_tc.bpf.c");
    let inc_dir = manifest_dir.join("../../ebpf-c");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let clang_out = out_dir.join("hyperion_xdp_tc_clang.bpf.o");

    if c_src.exists() {
        let status = Command::new("clang")
            .args([
                "-O2",
                "-g",
                "-Wall",
                "-Werror",
                "-target",
                "bpf",
                "-D__BPF__",
                "-D__TARGET_ARCH_x86",
                "-I",
            ])
            .arg(&inc_dir)
            .arg("-c")
            .arg(&c_src)
            .arg("-o")
            .arg(&clang_out)
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("cargo:rustc-cfg=has_clang_bpf_obj");
            }
            _ => {
                // Portable container/Windows fallback: `elf_bpf` emits verified ELF64 EM_BPF bytecode.
            }
        }
    }
}
