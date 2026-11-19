# syntax=docker/dockerfile:1.7
# Minimal multi-stage non-root ONNX Runtime image for HYPERION Channel 05 (services/sentinel-ml)
FROM python:3.12-slim AS builder
WORKDIR /build

COPY contracts/gen/python /build/contracts/gen/python
COPY services/sentinel-ml /build/services/sentinel-ml

RUN python -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir --upgrade pip \
    && /opt/venv/bin/pip install --no-cache-dir \
       /build/contracts/gen/python \
       numpy onnxruntime protobuf grpcio confluent-kafka fastapi uvicorn prometheus-client pydantic \
    && /opt/venv/bin/pip install --no-cache-dir --no-deps /build/services/sentinel-ml

FROM python:3.12-slim AS runtime
WORKDIR /app

ENV PATH="/opt/venv/bin:$PATH" \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    SENTINEL_MODELS_DIR=/app/services/sentinel-ml/models \
    SENTINEL_GRPC_PORT=50153 \
    SENTINEL_HTTP_PORT=8084

RUN groupadd -g 65532 hyperion \
    && useradd -u 65532 -g 65532 -M -s /usr/sbin/nologin hyperion

COPY --from=builder /opt/venv /opt/venv
COPY --chown=65532:65532 services/sentinel-ml/models /app/services/sentinel-ml/models
COPY --chown=65532:65532 services/sentinel-ml/src /app/services/sentinel-ml/src

USER 65532:65532
WORKDIR /app/services/sentinel-ml
EXPOSE 50153 8084

HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8084/healthz', timeout=2)"

CMD ["python", "-m", "sentinel_ml.cli", "serve"]
