# syntax=docker/dockerfile:1.7
# Multi-stage production Dockerfile for HYPERION Channel 04 (services/stream-aggregator)
# Build context: monorepo root (e.g. `docker build -f services/stream-aggregator/Dockerfile .`)

FROM rust:1.85-bookworm AS builder
WORKDIR /workspace

RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler && rm -rf /var/lib/apt/lists/*

COPY proto ./proto
COPY contracts/gen/rust ./contracts/gen/rust
COPY services/stream-aggregator ./services/stream-aggregator

WORKDIR /workspace/services/stream-aggregator
RUN cargo build --release --bin stream-aggregator

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /workspace/services/stream-aggregator/target/release/stream-aggregator /usr/local/bin/stream-aggregator

ENV HYPERION_AGGREGATOR_GRPC_ADDR="0.0.0.0:50152" \
    HYPERION_AGGREGATOR_HTTP_ADDR="0.0.0.0:8083" \
    RUST_LOG="info,hyperion_stream_aggregator=info"

EXPOSE 50152 8083
ENTRYPOINT ["/usr/local/bin/stream-aggregator"]
