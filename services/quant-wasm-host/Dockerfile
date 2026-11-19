# syntax=docker/dockerfile:1.7
# HYPERION OMEGA Channel 15 — Co-Located Deterministic Wasmtime Quant Host
# Multi-stage production build matching `docker-compose.omega.yml` (context = monorepo root).

FROM rust:1.86-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        protobuf-compiler pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add wasm32-unknown-unknown

WORKDIR /workspace
COPY proto /workspace/proto
COPY services/quant-wasm-host /workspace/services/quant-wasm-host

WORKDIR /workspace/services/quant-wasm-host
RUN cargo build --release --target wasm32-unknown-unknown \
        -p stat-arb-zscore -p obi-sniper -p delta-hedger -p malware-bombs \
    && mkdir -p artifacts \
    && cp target/wasm32-unknown-unknown/release/stat_arb_zscore.wasm artifacts/ \
    && cp target/wasm32-unknown-unknown/release/obi_sniper.wasm artifacts/ \
    && cp target/wasm32-unknown-unknown/release/delta_hedger.wasm artifacts/ \
    && cp target/wasm32-unknown-unknown/release/malware_bombs.wasm artifacts/ \
    && cargo build --release -p quant-wasm-host --bin quant-wasm-host

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 hyperion

WORKDIR /app
COPY --from=builder /workspace/services/quant-wasm-host/target/release/quant-wasm-host /usr/local/bin/quant-wasm-host
COPY --from=builder /workspace/services/quant-wasm-host/artifacts /app/artifacts

USER hyperion
ENV GRPC_PORT=50164 \
    HTTP_PORT=8094 \
    WASM_MAX_LINEAR_MEMORY_BYTES=16777216 \
    WASM_MAX_FUEL_PER_TICK=50000

EXPOSE 50164 8094
HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
    CMD curl -fsS http://127.0.0.1:8094/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/quant-wasm-host"]
