set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

default:
    @just --list

# Validate Protobuf (buf lint), OpenAPI 3.1, Binary WS, TimescaleDB SQL, and Docker Compose
lint:
    buf lint proto
    python scripts/validate_contracts.py

# Generate deterministic Protobuf & gRPC bindings for Rust, Go, Python, and TypeScript
codegen:
    python scripts/codegen.py

# Compile and run contract unit tests across all 4 target languages (Rust, Go, Python, TS)
test-contracts:
    cargo clippy --manifest-path contracts/gen/rust/Cargo.toml --all-targets -- -D warnings
    cargo test --manifest-path contracts/gen/rust/Cargo.toml
    go vet -C contracts/gen/go ./...
    go test -C contracts/gen/go -v ./...
    python -m unittest discover -s contracts/gen/python/tests -p "test_*.py" -v
    npm --prefix contracts/gen/ts run typecheck
    npm --prefix contracts/gen/ts test

# Validate docker-compose.infra.yml syntax and port rules
infra-config:
    docker compose -f docker-compose.infra.yml config --quiet

# Spin up local production infrastructure (Redpanda, TimescaleDB, Redis 7, OTel, Prometheus, Jaeger)
infra-up:
    docker compose -f docker-compose.infra.yml up -d

# Tear down local infrastructure containers
infra-down:
    docker compose -f docker-compose.infra.yml down

# Probe readiness of all local infrastructure endpoints
health:
    python scripts/healthcheck.py

# Full Channel 01 verification loop (lint + 4-language codegen + 4-language compile & test)
verify: lint codegen test-contracts
