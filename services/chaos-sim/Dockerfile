# syntax=docker/dockerfile:1.7
FROM golang:1.24-alpine AS builder
WORKDIR /workspace

COPY contracts/gen/go ./contracts/gen/go
COPY services/chaos-sim/go.mod services/chaos-sim/go.sum* ./services/chaos-sim/
WORKDIR /workspace/services/chaos-sim
RUN go mod download

COPY services/chaos-sim ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags="-s -w" -o /out/chaos-sim ./cmd/chaos-sim

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=builder /out/chaos-sim /usr/local/bin/chaos-sim
EXPOSE 8086 19001 19002 19003 19004 19005
ENV CHAOS_SIM_LISTEN=:8086
ENTRYPOINT ["/usr/local/bin/chaos-sim"]
