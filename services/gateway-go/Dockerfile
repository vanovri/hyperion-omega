# Multi-stage minimal production image for HYPERION Channel 03 (services/gateway-go)
FROM golang:1.24-alpine AS builder
WORKDIR /src/services/gateway-go

COPY contracts/gen/go /src/contracts/gen/go
COPY services/gateway-go/go.mod services/gateway-go/go.sum ./
RUN go mod download

COPY services/gateway-go/ ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags="-s -w" -o /out/hyperion-gateway ./cmd/gateway

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=builder /out/hyperion-gateway /hyperion-gateway
EXPOSE 8080 8081
USER nonroot:nonroot
ENTRYPOINT ["/hyperion-gateway"]
