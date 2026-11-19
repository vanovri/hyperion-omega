# HYPERION OMEGA — Channel 16: eBPF/XDP Zero-Trust Packet Filter & Socket Telemetry
# Supports native Linux `clang -target bpf` kernel compilation + portable Rust/ELF64 BPF VM.

CLANG ?= clang
BPF_CFLAGS := -O2 -g -Wall -Werror -target bpf -D__BPF__ -D__TARGET_ARCH_x86 -Iebpf-c

.PHONY: all bpf-c bpf-elf test clippy bench clean

all: bpf-elf test

bpf-c:
	@mkdir -p artifacts
	$(CLANG) $(BPF_CFLAGS) -c ebpf-c/hyperion_xdp_tc.bpf.c -o artifacts/hyperion_xdp_tc.bpf.o

bpf-elf:
	cargo run --release -p netguard-daemon -- --emit-bpf-obj artifacts/hyperion_netguard.bpf.o

test:
	cargo test --workspace --release -- --nocapture

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

bench:
	cargo run --release -p netguard-daemon -- --bench
	cargo bench -p netguard-daemon
