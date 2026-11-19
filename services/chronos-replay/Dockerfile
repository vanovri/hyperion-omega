# HYPERION Channel 08 — Chronos Time-Travel Debugger & Counterfactual Timeline Fork Engine
# Build context: repository root (`docker build -f services/chronos-replay/Dockerfile .`)
FROM rust:1.85-bookworm AS builder

WORKDIR /workspace

COPY proto ./proto
COPY contracts/gen/rust ./contracts/gen/rust
COPY services/chronos-replay ./services/chronos-replay

WORKDIR /workspace/services/chronos-replay
RUN cargo build --release --bin chronos-replay

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -s /usr/sbin/nologin hyperion

COPY --from=builder /workspace/services/chronos-replay/target/release/chronos-replay /usr/local/bin/chronos-replay

USER hyperion

EXPOSE 50154 8088

ENV RUST_LOG=info \
    HYPERION_CHRONOS_REPLAY_GRPC_ADDR=0.0.0.0:50154 \
    HYPERION_CHRONOS_REPLAY_HTTP_PORT=8088 \
    HYPERION_DATA_DIR=/var/lib/hyperion/ledger

HEALTHCHECK --interval=5s --timeout=3s --retries=20 \
    CMD wget -qO- http://127.0.0.1:8088/healthz >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/usr/local/bin/chronos-replay"]
