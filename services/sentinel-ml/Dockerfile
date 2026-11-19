# syntax=docker/dockerfile:1
# Production lightweight ONNX Runtime image for HYPERION Sentinel-ML (Channel 05)
FROM python:3.12-slim AS runtime

WORKDIR /app

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    SENTINEL_MODELS_DIR=/app/services/sentinel-ml/models \
    SENTINEL_GRPC_PORT=50153 \
    SENTINEL_HTTP_PORT=8084

COPY contracts/gen/python /app/contracts/gen/python
COPY services/sentinel-ml /app/services/sentinel-ml

RUN pip install --no-cache-dir \
    /app/contracts/gen/python \
    numpy onnxruntime protobuf grpcio confluent-kafka fastapi uvicorn prometheus-client pydantic \
    && pip install --no-cache-dir --no-deps /app/services/sentinel-ml

WORKDIR /app/services/sentinel-ml
EXPOSE 50153 8084

HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8084/healthz', timeout=2)"

CMD ["python", "-m", "sentinel_ml.cli", "serve"]
