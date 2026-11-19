#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! HYPERION OMEGA — Channel 16: `netguard-daemon` Library Root.

#[allow(clippy::all, dead_code)]
pub mod pb {
    tonic::include_proto!("hyperion.omega.netguard.v1");
}

pub mod aegis_bridge;
pub mod ebpf_vm;
pub mod elf_bpf;
pub mod lpm;
pub mod packet_forge;
pub mod pipeline;
pub mod ratelimit;
pub mod server;
pub mod telemetry;

/// Canonical gRPC Control & Telemetry port (`01-hyperion-omega.md` §2).
pub const CANONICAL_GRPC_PORT: u16 = 50165;
/// Canonical HTTP / Prometheus `/metrics` / `/healthz` port (`01-hyperion-omega.md` §2).
pub const CANONICAL_HTTP_PORT: u16 = 8095;
