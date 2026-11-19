# Multi-stage production Dockerfile for HYPERION CH12 (`services/consensus-raft-rs`)
# Build context: repository root (`.`) per `docker-compose.omega.yml`

FROM rust:1.85-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
COPY proto ./proto
COPY services/consensus-raft-rs ./services/consensus-raft-rs

WORKDIR /workspace/services/consensus-raft-rs
RUN cargo build --release --bin consensus-raft-rs

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home hyperion

COPY --from=builder /workspace/services/consensus-raft-rs/target/release/consensus-raft-rs /usr/local/bin/consensus-raft-rs

USER hyperion
ENV GRPC_PORT=50161 \
    HTTP_PORT=8091 \
    RUST_LOG=consensus_raft_rs=info

EXPOSE 50161 8091 50201 50202 50203 50204 50205

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=5 \
    CMD curl -fsS http://127.0.0.1:${HTTP_PORT}/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/consensus-raft-rs"]
