# syntax=docker/dockerfile:1.7
# Minimal multi-stage non-root production image for HYPERION Channel 04 (services/stream-aggregator)
FROM rust:1.86-bookworm AS builder
WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler cmake pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY contracts/gen/rust ./contracts/gen/rust
COPY services/stream-aggregator ./services/stream-aggregator

WORKDIR /workspace/services/stream-aggregator
RUN cargo build --release --bin stream-aggregator \
    && strip /workspace/services/stream-aggregator/target/release/stream-aggregator

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 65532 hyperion \
    && useradd -u 65532 -g 65532 -M -s /usr/sbin/nologin hyperion

COPY --from=builder /workspace/services/stream-aggregator/target/release/stream-aggregator /usr/local/bin/stream-aggregator
USER 65532:65532
EXPOSE 50152 8083
ENV HYPERION_AGGREGATOR_GRPC_ADDR=0.0.0.0:50152 \
    HYPERION_AGGREGATOR_HTTP_ADDR=0.0.0.0:8083

HEALTHCHECK --interval=5s --timeout=3s --retries=10 \
    CMD wget -qO- http://127.0.0.1:8083/healthz >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/usr/local/bin/stream-aggregator"]
