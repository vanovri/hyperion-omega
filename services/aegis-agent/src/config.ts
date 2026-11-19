/**
 * HYPERION Channel 06 — Canonical Service & Runtime Configuration (`00-hyperion-contract.md` §3).
 */

export const DEFAULT_DEV_HMAC_SECRET = "hyperion-aegis-hmac-sha256-shared-secret-v1";
export const DEFAULT_DEV_JWT_SECRET = "hyperion-institutional-jwt-dev-secret-2026";

export interface AegisRuntimeConfig {
  readonly environment: "development" | "staging" | "production";
  readonly serviceName: string;
  readonly serviceVersion: string;
  readonly httpPort: number;
  readonly transportMode: "http" | "stdio";
  readonly gatewayHttpUrl: string;
  readonly gatewayMetricsUrl: string;
  readonly ledgerMetricsUrl: string;
  readonly streamAggregatorHttpUrl: string;
  readonly sentinelMlHttpUrl: string;
  readonly chaosSimHttpUrl: string;
  readonly chronosReplayHttpUrl: string;
  readonly prometheusUrl: string;
  readonly redpandaPandaproxyUrl: string;
  readonly redpandaAdminUrl: string;
  readonly reasoningPreference: "auto" | "gemini" | "openai" | "deterministic";
  readonly upstreamMode: "hybrid" | "simulation";
  readonly geminiApiKey?: string;
  readonly geminiModel: string;
  readonly openAiApiKey?: string;
  readonly openAiBaseUrl: string;
  readonly openAiModel: string;
  readonly ed25519SeedHex?: string;
  readonly hmacSecret: string;
  readonly jwtSecret: string;
  readonly internalAuthToken?: string;
  readonly corsOrigins: readonly string[];
  readonly upstreamTimeoutMs: number;
}

export function loadAegisConfig(env: NodeJS.ProcessEnv = process.env): AegisRuntimeConfig {
  const rawEnv = (env.HYPERION_ENV ?? "development").toLowerCase();
  const environment: AegisRuntimeConfig["environment"] =
    rawEnv === "production" || rawEnv === "staging" ? rawEnv : "development";

  const portRaw = Number.parseInt(env.HYPERION_AEGIS_AGENT_HTTP_PORT ?? env.PORT ?? "8085", 10);
  const httpPort = Number.isFinite(portRaw) && portRaw > 0 ? portRaw : 8085;

  const gatewayPort = env.HYPERION_GATEWAY_HTTP_PORT ?? "8080";
  const gatewayMetricsPort = env.HYPERION_GATEWAY_METRICS_PORT ?? "8081";
  const ledgerMetricsPort = env.HYPERION_LEDGER_METRICS_PORT ?? "8082";
  const aggregatorPort = env.HYPERION_STREAM_AGGREGATOR_HTTP_PORT ?? "8083";
  const sentinelPort = env.HYPERION_SENTINEL_ML_HTTP_PORT ?? "8084";
  const chaosPort = env.HYPERION_CHAOS_SIM_HTTP_PORT ?? "8086";
  const chronosPort = env.HYPERION_CHRONOS_REPLAY_HTTP_PORT ?? "8088";

  const transportEnv = (env.AEGIS_TRANSPORT ?? "http").toLowerCase();
  const transportMode: "http" | "stdio" = transportEnv === "stdio" ? "stdio" : "http";

  const prefEnv = (env.AEGIS_REASONING_MODE ?? "auto").toLowerCase();
  const reasoningPreference: AegisRuntimeConfig["reasoningPreference"] =
    prefEnv === "gemini" || prefEnv === "openai" || prefEnv === "deterministic" ? prefEnv : "auto";

  const upstreamModeEnv = (env.AEGIS_UPSTREAM_MODE ?? "hybrid").toLowerCase();
  const upstreamMode: AegisRuntimeConfig["upstreamMode"] =
    upstreamModeEnv === "simulation" ? "simulation" : "hybrid";

  const requestedGeminiModel = env.AEGIS_GEMINI_MODEL ?? "gemini-3.8-flash";
  // Enforce current Gemini model family (never legacy 1.5/2.0/2.5)
  const geminiModel = /^(gemini-1\.|gemini-2\.)/i.test(requestedGeminiModel)
    ? "gemini-3.8-flash"
    : requestedGeminiModel;

  const hmacSecret = env.AEGIS_HMAC_SECRET ?? DEFAULT_DEV_HMAC_SECRET;
  const jwtSecret = env.HYPERION_JWT_SECRET ?? DEFAULT_DEV_JWT_SECRET;
  const internalAuthToken = env.HYPERION_INTERNAL_AUTH_TOKEN?.trim() || undefined;

  if (environment === "production") {
    if (hmacSecret === DEFAULT_DEV_HMAC_SECRET || hmacSecret.length < 32) {
      throw new Error(
        "SECURITY_FAIL_CLOSED: AEGIS_HMAC_SECRET must be set to a >=32-byte non-default secret in production."
      );
    }
    if (jwtSecret === DEFAULT_DEV_JWT_SECRET || jwtSecret.length < 32) {
      throw new Error(
        "SECURITY_FAIL_CLOSED: HYPERION_JWT_SECRET must be set to a >=32-byte non-default secret in production."
      );
    }
    if (!internalAuthToken || internalAuthToken.length < 24) {
      throw new Error(
        "SECURITY_FAIL_CLOSED: HYPERION_INTERNAL_AUTH_TOKEN (>=24 chars) is required in production."
      );
    }
  }

  const rawCors = env.HYPERION_CORS_ORIGINS?.trim();
  const corsOrigins = rawCors
    ? rawCors
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean)
    : ["http://localhost:3000", "http://127.0.0.1:3000", "http://localhost:3001", "http://127.0.0.1:3001"];

  return {
    environment,
    serviceName: "aegis-agent",
    serviceVersion: "1.0.0",
    httpPort,
    transportMode,
    gatewayHttpUrl: env.HYPERION_GATEWAY_URL ?? `http://127.0.0.1:${gatewayPort}`,
    gatewayMetricsUrl: env.HYPERION_GATEWAY_METRICS_URL ?? `http://127.0.0.1:${gatewayMetricsPort}`,
    ledgerMetricsUrl: env.HYPERION_LEDGER_METRICS_URL ?? `http://127.0.0.1:${ledgerMetricsPort}`,
    streamAggregatorHttpUrl:
      env.HYPERION_STREAM_AGGREGATOR_URL ?? `http://127.0.0.1:${aggregatorPort}`,
    sentinelMlHttpUrl: env.HYPERION_SENTINEL_ML_URL ?? `http://127.0.0.1:${sentinelPort}`,
    chaosSimHttpUrl: env.HYPERION_CHAOS_SIM_URL ?? `http://127.0.0.1:${chaosPort}`,
    chronosReplayHttpUrl: env.HYPERION_CHRONOS_REPLAY_URL ?? `http://127.0.0.1:${chronosPort}`,
    prometheusUrl: env.HYPERION_PROMETHEUS_URL ?? "http://127.0.0.1:9090",
    redpandaPandaproxyUrl: env.HYPERION_REDPANDA_PANDAPROXY_URL ?? "http://127.0.0.1:18082",
    redpandaAdminUrl: env.HYPERION_REDPANDA_ADMIN_URL ?? "http://127.0.0.1:19644",
    reasoningPreference,
    upstreamMode,
    geminiApiKey: env.GEMINI_API_KEY ?? env.GOOGLE_API_KEY,
    geminiModel,
    openAiApiKey: env.OPENAI_API_KEY,
    openAiBaseUrl: env.OPENAI_BASE_URL ?? "https://api.openai.com/v1",
    openAiModel: env.AEGIS_OPENAI_MODEL ?? "gpt-4.1-mini",
    ed25519SeedHex: env.AEGIS_ED25519_SEED_HEX,
    hmacSecret,
    jwtSecret,
    internalAuthToken,
    corsOrigins,
    upstreamTimeoutMs: Number.parseInt(env.AEGIS_UPSTREAM_TIMEOUT_MS ?? "2000", 10) || 2000
  };
}
