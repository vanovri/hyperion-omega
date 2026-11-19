# syntax=docker/dockerfile:1
FROM golang:1.24-alpine AS builder
WORKDIR /workspace
COPY contracts/gen/go ./contracts/gen/go
COPY proto/omega/gen/go ./proto/omega/gen/go
COPY services/chain-relayer-go ./services/chain-relayer-go
WORKDIR /workspace/services/chain-relayer-go
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/chain-relayer ./cmd/chain-relayer

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=builder /out/chain-relayer /chain-relayer
EXPOSE 50167 8097
USER nonroot:nonroot
ENTRYPOINT ["/chain-relayer"]
