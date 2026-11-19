# ==============================================================================
# HYPERION OMEGA — CHANNEL 19 (`services/alpha-evo-py`)
# Multi-stage Python 3.12 Quantitative AI & Symbolic Alpha Research Container
# Built with monorepo root context (`docker-compose.omega.yml`: context `.`)
# ==============================================================================
FROM python:3.12-slim AS builder

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1

WORKDIR /build
COPY services/alpha-evo-py/pyproject.toml services/alpha-evo-py/README.md ./
COPY services/alpha-evo-py/src/ ./src/

RUN python -m venv /opt/venv \
    && /opt/venv/bin/pip install --upgrade pip \
    && /opt/venv/bin/pip install --extra-index-url https://download.pytorch.org/whl/cpu .

FROM python:3.12-slim AS runtime

RUN groupadd -g 10019 hyperion && useradd -u 10019 -g hyperion -m -s /sbin/nologin hyperion

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PATH="/opt/venv/bin:${PATH}" \
    HTTP_PORT=8098 \
    GRPC_PORT=50168 \
    ALPHA_EVO_ARTIFACTS_DIR=/app/artifacts

WORKDIR /app
COPY --from=builder /opt/venv /opt/venv
COPY --chown=hyperion:hyperion services/alpha-evo-py/src/ ./src/
COPY --chown=hyperion:hyperion services/alpha-evo-py/artifacts/ ./artifacts/

USER hyperion
EXPOSE 8098 50168

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8098/healthz', timeout=2)"

CMD ["alpha-evo-serve"]
