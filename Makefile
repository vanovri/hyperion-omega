.PHONY: help lint codegen test-contracts infra-config infra-up infra-down health verify

help:
	@echo "HYPERION Root Makefile Targets:"
	@echo "  make lint            - Validate Protobuf (buf lint), OpenAPI 3.1, SQL, and Docker Compose"
	@echo "  make codegen         - Generate bindings for Rust, Go, Python, and TypeScript"
	@echo "  make test-contracts  - Compile & test generated SDKs across all 4 languages"
	@echo "  make infra-up        - Start Redpanda, TimescaleDB, Redis 7, OTel, Prometheus, Jaeger"
	@echo "  make infra-down      - Stop local infrastructure stack"
	@echo "  make health          - Run automated infrastructure readiness healthcheck"
	@echo "  make verify          - Run full lint + codegen + 4-language compile/test loop"

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
