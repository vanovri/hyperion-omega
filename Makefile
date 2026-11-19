.PHONY: help lint codegen test-contracts infra-config infra-up infra-down health verify \
        deploy-lint up down demo crucible crucible-overclock check-links verify-all \
        omega-config omega-core-up omega-full-up omega-down omega-studio-dev omega-studio-verify \
        omega-singularity omega-mutate omega-verify

help:
	@echo "HYPERION & HYPERION OMEGA Root Makefile Targets:"
	@echo "  make up                   - One-command full HYPERION 9-subsystem launch (Docker Compose or native fallback)"
	@echo "  make down                 - Tear down full HYPERION stack cleanly"
	@echo "  make demo                 - One-command live demo: launch stack + run 3-phase E2E Crucible + SVGs"
	@echo "  make deploy-lint          - Validate docker-compose.full.yml + Helm chart (helm lint --strict)"
	@echo "  make crucible             - Run live 3-phase E2E Chaos Crucible + k6 + measured benchmark SVGs"
	@echo "  make crucible-overclock   - Run CH10 unit tests + 25-mutant audit + 500k soak & 3-pass bench"
	@echo "  make check-links          - Verify zero broken documentation links across repo"
	@echo "  make verify-all           - Run full Channel 10 Zero-Defect gate across all stacks & docs"
	@echo "  make lint                 - Validate Protobuf (buf lint), OpenAPI 3.1, SQL, and Docker Compose"
	@echo "  make codegen              - Generate bindings for Rust, Go, Python, and TypeScript"
	@echo "  make test-contracts       - Compile & test generated SDKs across all 4 languages"
	@echo "  make infra-up             - Start Redpanda, TimescaleDB, Redis 7, OTel, Prometheus, Jaeger"
	@echo "  make infra-down           - Stop local infrastructure stack"
	@echo "  make health               - Run automated infrastructure readiness healthcheck"
	@echo "  make verify               - Run full CH01 lint + codegen + 4-language compile/test loop"
	@echo "  make omega-config         - Validate docker-compose.omega.yml (core & full-omega profiles)"
	@echo "  make omega-core-up        - Start RAM-friendly core profile (Infra + Ledger + Gateway + Sovereign Studio)"
	@echo "  make omega-full-up        - Start full 20-channel Omega Singularity profile"
	@echo "  make omega-down           - Tear down Omega Singularity stack"
	@echo "  make omega-studio-dev     - Start Sovereign Studio web UI on http://localhost:3001"
	@echo "  make omega-studio-verify  - Run Sovereign Studio tsc + Vitest + 500k V8 soak + Next.js build"
	@echo "  make omega-singularity    - Execute 20-Channel Black Swan Singularity Crucible"
	@echo "  make omega-mutate         - Execute 26-Mutant Schemata Audit (>=95% kill gate)"
	@echo "  make omega-verify         - Run full Channel 20 Zero-Defect Grand Finale gate"

lint:
	buf lint proto
	python scripts/validate_contracts.py

codegen:
	python scripts/codegen.py

test-contracts:
	cargo clippy --manifest-path contracts/gen/rust/Cargo.toml --all-targets -- -D warnings
	cargo test --manifest-path contracts/gen/rust/Cargo.toml
	go vet -C contracts/gen/go ./...
	go test -C contracts/gen/go -v ./...
	python -m unittest discover -s contracts/gen/python/tests -p "test_*.py" -v
	npm --prefix contracts/gen/ts run typecheck
	npm --prefix contracts/gen/ts test

infra-config:
	docker compose -f docker-compose.infra.yml config --quiet

infra-up:
	docker compose -f docker-compose.infra.yml up -d

infra-down:
	docker compose -f docker-compose.infra.yml down

health:
	python scripts/healthcheck.py

verify: lint codegen test-contracts

deploy-lint:
	docker compose -f docker-compose.full.yml config --quiet
	helm lint deploy/helm/hyperion --strict

up:
	python scripts/hyperion_up.py up

down:
	python scripts/hyperion_up.py down

demo:
	python scripts/hyperion_up.py demo

crucible:
	python e2e/run_crucible.py

crucible-overclock:
	python -m unittest e2e/test_crucible_core.py -v
	python e2e/mutation_audit_ch10.py
	python e2e/soak_and_bench_ch10.py

check-links:
	python scripts/check_links.py

verify-all: lint deploy-lint test-contracts crucible-overclock crucible check-links

omega-config:
	docker compose -f docker-compose.omega.yml --profile core config --quiet
	docker compose -f docker-compose.omega.yml --profile full-omega config --quiet

omega-core-up:
	docker compose -f docker-compose.omega.yml --profile core up -d

omega-full-up:
	docker compose -f docker-compose.omega.yml --profile full-omega up -d

omega-down:
	docker compose -f docker-compose.omega.yml --profile full-omega down

omega-studio-dev:
	npm --prefix apps/sovereign-studio run dev

omega-studio-verify:
	npm --prefix apps/sovereign-studio run typecheck
	npm --prefix apps/sovereign-studio test
	node --expose-gc apps/sovereign-studio/scripts/soak-500k.mjs
	npm --prefix apps/sovereign-studio run build

omega-singularity:
	python e2e-omega/run_omega_singularity.py --profile full-omega

omega-mutate:
	python e2e-omega/mutation_audit_runner.py

omega-verify: omega-config omega-singularity omega-mutate omega-studio-verify
