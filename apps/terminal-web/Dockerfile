# syntax=docker/dockerfile:1.7
# Minimal multi-stage non-root production image for HYPERION Channel 09 (apps/terminal-web)
FROM node:22-alpine AS builder
WORKDIR /workspace

COPY contracts/gen/ts ./contracts/gen/ts
RUN cd /workspace/contracts/gen/ts && npm install --no-audit --no-fund && npm run build --if-present

COPY apps/terminal-web/package.json apps/terminal-web/package-lock.json* ./apps/terminal-web/
WORKDIR /workspace/apps/terminal-web
RUN npm ci --no-audit --no-fund || npm install --no-audit --no-fund

COPY apps/terminal-web ./
ENV NEXT_TELEMETRY_DISABLED=1
RUN npm run build

FROM node:22-alpine AS runtime
WORKDIR /app/apps/terminal-web
ENV NODE_ENV=production \
    NEXT_TELEMETRY_DISABLED=1 \
    PORT=3000 \
    HOSTNAME=0.0.0.0

COPY --from=builder --chown=node:node /workspace/contracts/gen/ts /app/contracts/gen/ts
COPY --from=builder --chown=node:node /workspace/apps/terminal-web/package.json ./package.json
COPY --from=builder --chown=node:node /workspace/apps/terminal-web/node_modules ./node_modules
COPY --from=builder --chown=node:node /workspace/apps/terminal-web/.next ./.next
COPY --from=builder --chown=node:node /workspace/apps/terminal-web/next.config.ts ./next.config.ts

USER node
EXPOSE 3000
HEALTHCHECK --interval=5s --timeout=3s --retries=15 \
    CMD node -e "fetch('http://127.0.0.1:3000').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["./node_modules/.bin/next", "start", "--hostname", "0.0.0.0", "--port", "3000"]
