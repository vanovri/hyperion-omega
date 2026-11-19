import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { loadAegisConfig } from "../src/config.js";
import {
  DeterministicNanosecondClock,
  createAegisRuntime,
  type AegisRuntime
} from "../src/index.js";

describe("Security Hardening — SEC-04 Internal Token Guard & SEC-05 Fail-Closed Production Config", () => {
  const INTERNAL_SECRET = "hyperion-prod-internal-token-verify-987654321";
  let runtime: AegisRuntime;
  let closeServer: () => Promise<void>;
  let baseUrl = "";

  beforeAll(async () => {
    const testEnv: NodeJS.ProcessEnv = {
      HYPERION_ENV: "development",
      HYPERION_INTERNAL_AUTH_TOKEN: INTERNAL_SECRET,
      HYPERION_GATEWAY_METRICS_URL: "http://gateway-go:8081",
      HYPERION_LEDGER_METRICS_URL: "http://ledger-rs:8082",
      HYPERION_STREAM_AGGREGATOR_URL: "http://stream-aggregator:8083",
      HYPERION_CHAOS_SIM_URL: "http://chaos-sim:8086",
      HYPERION_CHRONOS_REPLAY_URL: "http://chronos-replay:8088"
    };
    const config = loadAegisConfig(testEnv);
    expect(config.gatewayMetricsUrl).toBe("http://gateway-go:8081");
    expect(config.chronosReplayHttpUrl).toBe("http://chronos-replay:8088");

    runtime = createAegisRuntime({
      env: testEnv,
      clock: new DeterministicNanosecondClock(),
      offlineSimulationOnly: true
    });
    const { server, port } = await runtime.startHttpServer(0);
    baseUrl = `http://127.0.0.1:${port}`;
    closeServer = async () => {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      });
    };
  });

  afterAll(async () => {
    await closeServer();
  });

  it("refuses production startup when dev default secrets or missing internal token are configured (SEC-05)", () => {
    expect(() =>
      loadAegisConfig({
        HYPERION_ENV: "production"
      })
    ).toThrow(/SECURITY_FAIL_CLOSED/);

    expect(() =>
      loadAegisConfig({
        HYPERION_ENV: "production",
        AEGIS_HMAC_SECRET: "a".repeat(40),
        HYPERION_JWT_SECRET: "b".repeat(40)
      })
    ).toThrow(/HYPERION_INTERNAL_AUTH_TOKEN/);

    const validProd = loadAegisConfig({
      HYPERION_ENV: "production",
      AEGIS_HMAC_SECRET: "a".repeat(40),
      HYPERION_JWT_SECRET: "b".repeat(40),
      HYPERION_INTERNAL_AUTH_TOKEN: "c".repeat(32)
    });
    expect(validProd.environment).toBe("production");
  });

  it("blocks unauthenticated POST /mcp and POST /api/v1/aegis/investigations with 401 while keeping /healthz readable (SEC-04)", async () => {
    const health = await fetch(`${baseUrl}/healthz`);
    expect(health.status).toBe(200);

    const noTokMcp = await fetch(`${baseUrl}/mcp`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} })
    });
    expect(noTokMcp.status).toBe(401);

    const badTokInv = await fetch(`${baseUrl}/api/v1/aegis/investigations`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Hyperion-Internal-Token": "wrong-token"
      },
      body: JSON.stringify({})
    });
    expect(badTokInv.status).toBe(401);

    const okMcp = await fetch(`${baseUrl}/mcp`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        "X-Hyperion-Internal-Token": INTERNAL_SECRET
      },
      body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} })
    });
    expect(okMcp.status).toBe(200);
  });
});
