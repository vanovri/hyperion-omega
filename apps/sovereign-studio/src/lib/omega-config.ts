/**
 * HYPERION OMEGA — Canonical 20-Channel Subsystem & Port Matrix
 * Locked per .agents/rules/00-hyperion-contract.md & .agents/rules/01-hyperion-omega.md
 */

export interface ChannelEndpointSpec {
  readonly channel: number;
  readonly name: string;
  readonly stack: string;
  readonly grpcPort: number | null;
  readonly httpPort: number | null;
  readonly healthUrl: string;
}

export const OMEGA_20_CHANNEL_MATRIX: ReadonlyArray<ChannelEndpointSpec> = [
  { channel: 1, name: "proto-contracts-infra", stack: "Protobuf v3 / Redpanda / TimescaleDB", grpcPort: 4317, httpPort: 19644, healthUrl: "http://localhost:19644/v1/status/ready" },
  { channel: 2, name: "ledger-rs", stack: "Rust LMAX LOB + Blake3 WAL", grpcPort: 50151, httpPort: 8082, healthUrl: "http://localhost:8082/healthz" },
  { channel: 3, name: "gateway-go", stack: "Go Ed25519 + Binary WS 0x4859", grpcPort: null, httpPort: 8080, healthUrl: "http://localhost:8081/healthz" },
  { channel: 4, name: "stream-aggregator", stack: "Rust Microstructure + TimescaleDB", grpcPort: 50152, httpPort: 8083, healthUrl: "http://localhost:8083/healthz" },
  { channel: 5, name: "sentinel-ml", stack: "Python 3.12 + ONNX INT8 + Graph", grpcPort: 50153, httpPort: 8084, healthUrl: "http://localhost:8084/healthz" },
  { channel: 6, name: "aegis-agent", stack: "TypeScript Custom MCP SRE", grpcPort: null, httpPort: 8085, healthUrl: "http://localhost:8085/healthz" },
  { channel: 7, name: "chaos-sim", stack: "Go 1,000+ Bot Simulator", grpcPort: null, httpPort: 8086, healthUrl: "http://localhost:8086/healthz" },
  { channel: 8, name: "chronos-replay", stack: "Rust Nanosecond Time-Travel", grpcPort: 50154, httpPort: 8088, healthUrl: "http://localhost:8088/healthz" },
  { channel: 9, name: "terminal-web", stack: "Next.js 60fps Canvas UI", grpcPort: null, httpPort: 3000, healthUrl: "http://localhost:3000" },
  { channel: 10, name: "core-deploy-e2e", stack: "Docker / Helm / k6 Crucible", grpcPort: null, httpPort: 9090, healthUrl: "http://localhost:9090/-/healthy" },
  { channel: 11, name: "formal-verification", stack: "Z3 SMT + TLA+ / 1M+ BFS", grpcPort: null, httpPort: null, healthUrl: "static://z3-unsat-verified" },
  { channel: 12, name: "consensus-raft-rs", stack: "Rust 5-Node Raft + PreVote + VOPR", grpcPort: 50161, httpPort: 8091, healthUrl: "http://localhost:8091/healthz" },
  { channel: 13, name: "zk-solvency-rs", stack: "Rust Arkworks BN254 + MSST", grpcPort: 50162, httpPort: 8092, healthUrl: "http://localhost:8092/healthz" },
  { channel: 14, name: "margin-risk-rs", stack: "Rust SIMD SoA SVI + SPAN + ADL", grpcPort: 50163, httpPort: 8093, healthUrl: "http://localhost:8093/healthz" },
  { channel: 15, name: "quant-wasm-host", stack: "Rust Wasmtime Zero-Syscall JIT", grpcPort: 50164, httpPort: 8094, healthUrl: "http://localhost:8094/healthz" },
  { channel: 16, name: "ebpf-netguard", stack: "eBPF/XDP + Rust Packet Shield", grpcPort: 50165, httpPort: 8095, healthUrl: "http://localhost:8095/healthz" },
  { channel: 17, name: "lakehouse-olap-rs", stack: "Rust Arrow / Parquet / DataFusion", grpcPort: 50166, httpPort: 8096, healthUrl: "http://localhost:8096/healthz" },
  { channel: 18, name: "evm-settlement-relayer", stack: "Solidity 0.8.26 + Go Relayer", grpcPort: 50167, httpPort: 8097, healthUrl: "http://localhost:8097/healthz" },
  { channel: 19, name: "alpha-evo-py", stack: "Python Symbolic GP + PPO + DSR", grpcPort: 50168, httpPort: 8098, healthUrl: "http://localhost:8098/healthz" },
  { channel: 20, name: "sovereign-studio", stack: "Next.js + WebGL2 GLSL + Tauri v2", grpcPort: null, httpPort: 3001, healthUrl: "http://localhost:3001" },
];
