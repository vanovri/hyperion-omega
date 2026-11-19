# syntax=docker/dockerfile:1.7
# Minimal multi-stage production image for HYPERION Channel 02 (services/ledger-rs)
FROM rust:1.86-bookworm AS builder
WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler cmake pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY contracts/gen/rust ./contracts/gen/rust
COPY services/ledger-rs ./services/ledger-rs

WORKDIR /workspace/services/ledger-rs
RUN cargo build --release --bin ledger-rs \
    && strip /workspace/services/ledger-rs/target/release/ledger-rs

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 65532 hyperion \
    && useradd -u 65532 -g 65532 -M -s /usr/sbin/nologin hyperion \
    && mkdir -p /var/lib/hyperion/ledger \
    && chown -R 65532:65532 /var/lib/hyperion

COPY --from=builder /workspace/services/ledger-rs/target/release/ledger-rs /usr/local/bin/ledger-rs
USER 65532:65532
EXPOSE 50151 8082
ENV HYPERION_LEDGER_GRPC_ADDR=0.0.0.0:50151 \
    HYPERION_LEDGER_METRICS_PORT=8082 \
    HYPERION_DATA_DIR=/var/lib/hyperion/ledger

HEALTHCHECK --interval=5s --timeout=3s --retries=10 \
    CMD wget -qO- http://127.0.0.1:8082/healthz >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/usr/local/bin/ledger-rs"]
