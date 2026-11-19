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

# ═══════════════════════════════════════════════════════════════════════════════
# HYPERION CORE (Channels 01–10) One-Command Orchestration & Crucible Targets
# ═══════════════════════════════════════════════════════════════════════════════

# Validate docker-compose.full.yml and production Helm chart strictly
deploy-lint:
    docker compose -f docker-compose.full.yml config --quiet
    helm lint deploy/helm/hyperion --strict

# One-command full stack startup (Docker Compose primary, automatic native compiled-binary mesh fallback)
up:
    python scripts/hyperion_up.py up

# Tear down full HYPERION stack (Docker Compose + native processes)
down:
    python scripts/hyperion_up.py down

# One-command recruiter demo: launch stack, run 3-phase E2E Chaos Crucible, generate SVGs, open terminal
demo:
    python scripts/hyperion_up.py demo

# Execute live 3-phase End-to-End Chaos Crucible + k6 load harness + benchmark SVG generation
crucible:
    python e2e/run_crucible.py

# Run CH10 Crucible unit tests, 25-mutant kill audit (>=95%), and 500k zero-leak soak + 3-pass bench
crucible-overclock:
    python -m unittest e2e/test_crucible_core.py -v
    python e2e/mutation_audit_ch10.py
    python e2e/soak_and_bench_ch10.py

# Verify zero broken Markdown / documentation links across README, docs, specs, and contracts
check-links:
    python scripts/check_links.py

# Full Channel 10 Zero-Defect verification gate across all 4 languages, Helm, Compose, Crucible, and Docs
verify-all: lint deploy-lint test-contracts crucible-overclock crucible check-links

# ═══════════════════════════════════════════════════════════════════════════════
# HYPERION OMEGA (Channels 11–20) Modular Orchestration & Verification Targets
# ═══════════════════════════════════════════════════════════════════════════════

# Validate both modular Docker Compose profiles (core vs full-omega)
omega-config:
    docker compose -f docker-compose.omega.yml --profile core config --quiet
    docker compose -f docker-compose.omega.yml --profile full-omega config --quiet

# Spin up RAM-friendly core stack (Infra + Ledger + Gateway + Sovereign Studio)
omega-core-up:
    docker compose -f docker-compose.omega.yml --profile core up -d

# Spin up full 20-channel Omega Singularity stack
omega-full-up:
    docker compose -f docker-compose.omega.yml --profile full-omega up -d

# Tear down Omega stack containers
omega-down:
    docker compose -f docker-compose.omega.yml --profile full-omega down

# Run Sovereign Studio (Next.js 15 + WebGL2 Shaders on :3001)
omega-studio-dev:
    npm --prefix apps/sovereign-studio run dev

# Typecheck, test, soak (500k cycles), and build Sovereign Studio
omega-studio-verify:
    npm --prefix apps/sovereign-studio run typecheck
    npm --prefix apps/sovereign-studio test
    node --expose-gc apps/sovereign-studio/scripts/soak-500k.mjs
    npm --prefix apps/sovereign-studio run build

# Run 20-Channel "Black Swan Singularity" Crucible (Rust harness + port probes + SVG)
omega-singularity:
    python e2e-omega/run_omega_singularity.py --profile full-omega

# Run 26-Mutant Schemata Financial/Consensus/Crypto Mutation Testing Audit (>=95% kill gate)
omega-mutate:
    python e2e-omega/mutation_audit_runner.py

# Full Channel 20 Grand Finale verification gate
omega-verify: omega-config omega-singularity omega-mutate omega-studio-verify
