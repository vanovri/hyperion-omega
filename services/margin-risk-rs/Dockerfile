# syntax=docker/dockerfile:1.7
FROM rust:1.86-slim-bookworm AS builder

WORKDIR /workspace
COPY contracts/gen/rust ./contracts/gen/rust
COPY proto/omega/gen/rust ./proto/omega/gen/rust
COPY services/margin-risk-rs ./services/margin-risk-rs

WORKDIR /workspace/services/margin-risk-rs
RUN cargo build --release --bin margin-risk-rs

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/services/margin-risk-rs/target/release/margin-risk-rs /usr/local/bin/margin-risk-rs

ENV MARGIN_GRPC_ADDR=0.0.0.0:50163
ENV MARGIN_HTTP_ADDR=0.0.0.0:8093
EXPOSE 50163 8093

ENTRYPOINT ["/usr/local/bin/margin-risk-rs"]
