# syntax=docker/dockerfile:1.7
# Minimal multi-stage non-root production image for HYPERION Channel 06 (services/aegis-agent)
FROM node:22-alpine AS builder
WORKDIR /workspace

COPY contracts/gen/ts ./contracts/gen/ts
RUN cd /workspace/contracts/gen/ts && npm install --no-audit --no-fund && npm run build --if-present

WORKDIR /workspace/services/aegis-agent
COPY services/aegis-agent/package.json services/aegis-agent/package-lock.json* ./
COPY services/aegis-agent/tsconfig.json ./tsconfig.json
COPY services/aegis-agent/tsconfig.build.json ./tsconfig.build.json
COPY services/aegis-agent/src ./src

RUN npm install --no-audit --no-fund && npm run build && npm prune --omit=dev

FROM node:22-alpine AS runtime
WORKDIR /app/services/aegis-agent
ENV NODE_ENV=production \
    HYPERION_AEGIS_AGENT_HTTP_PORT=8085

COPY --from=builder --chown=node:node /workspace/contracts/gen/ts /app/contracts/gen/ts
COPY --from=builder --chown=node:node /workspace/services/aegis-agent/package.json ./package.json
COPY --from=builder --chown=node:node /workspace/services/aegis-agent/node_modules ./node_modules
COPY --from=builder --chown=node:node /workspace/services/aegis-agent/dist ./dist

USER node
EXPOSE 8085
HEALTHCHECK --interval=5s --timeout=3s --retries=5 \
    CMD node -e "fetch('http://127.0.0.1:8085/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["node", "dist/index.js"]
