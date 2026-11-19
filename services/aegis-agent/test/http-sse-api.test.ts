import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  DeterministicNanosecondClock,
  createAegisRuntime,
  type AegisRuntime
} from "../src/index.js";

describe("HTTP Control Plane, Streamable HTTP /mcp & Live /events/incidents SSE", () => {
  let runtime: AegisRuntime;
  let closeServer: () => Promise<void>;
  let baseUrl = "";

  beforeAll(async () => {
    const clock = new DeterministicNanosecondClock();
    runtime = createAegisRuntime({
      clock,
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

  it("serves /healthz, /metrics, Streamable HTTP /mcp, live SSE /events/incidents, HITL approval & rollback", async () => {
    // 1. /healthz & /metrics
    const healthRes = await fetch(`${baseUrl}/healthz`);
    expect(healthRes.status).toBe(200);
    const healthJson = (await healthRes.json()) as { status: string; service: string };
    expect(healthJson.status).toBe("ok");
    expect(healthJson.service).toBe("aegis-agent");

    // 2. Connect real MCP Client over StreamableHTTPClientTransport to POST /mcp
    const mcpClient = new Client({ name: "http-mcp-verifier", version: "1.0.0" });
    const mcpHttpTransport = new StreamableHTTPClientTransport(new URL(`${baseUrl}/mcp`));
    await mcpClient.connect(mcpHttpTransport);
    const { tools } = await mcpClient.listTools();
    expect(tools).toHaveLength(4);
    await mcpClient.close();

    // 3. Open live SSE stream to GET /events/incidents
    const sseAbort = new AbortController();
    let invJson!: {
      incident_id: string;
      current_phase: string;
      pending_approvals: Array<{ approval_id: string }>;
      mitigation_outcomes: Array<{
        status: string;
        signed_command?: { rollback_token: string };
      }>;
    };

    try {
      const sseRes = await fetch(`${baseUrl}/events/incidents`, {
        signal: sseAbort.signal
      });
      expect(sseRes.status).toBe(200);
      expect(sseRes.headers.get("content-type")).toContain("text/event-stream");
      const reader = sseRes.body!.getReader();
      const decoder = new TextDecoder();
      let sseBuffer = "";

      const firstChunk = await reader.read();
      sseBuffer += decoder.decode(firstChunk.value, { stream: true });
      expect(sseBuffer).toContain("SSE_STREAM_CONNECTED");

      // 4. Trigger autonomous investigation over REST (`POST /api/v1/aegis/investigations`)
      // Use severity=96 + CRITICAL telemetry so HITL approval is also exercised via REST!
      runtime.simulationBackend.setTelemetryScenario({
        service_metrics: [
          {
            service: "ledger-rs",
            healthy: true,
            p50_latency_us: 3,
            p99_latency_us: 7,
            p999_latency_us: 12,
            error_rate_bps: 0,
            throughput_rps: 45_000
          }
        ],
        database_telemetry: {
          db_cluster: "timescaledb-pg16-primary",
          write_batch_p50_us: 15_000,
          write_batch_p99_us: 125_000, // CRITICAL -> proposes ENABLE_VOLATILITY_GUARD -> HITL!
          pool_saturation_bps: 9_100,
          active_connections: 88,
          hypertable_chunk_healthy: true
        },
        redpanda_consumer_lag: [],
        circuit_breakers: [],
        captured_ts_ns: "1795013580000000000"
      });

      const invRes = await fetch(`${baseUrl}/api/v1/aegis/investigations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          incident_id: "INC-HTTP-SSE-01",
          alert: {
            alert_id: "alert-http-01",
            engine_seq: "42500",
            symbol: "BTC-USDT",
            pattern_type: "WASH_TRADING_CYCLE",
            severity_score: 96,
            confidence_ppm: 980_000,
            offender_account_ids: ["9001", "9002", "9003"],
            detected_ts_ns: "1795013580000000000"
          }
        })
      });
      expect(invRes.status).toBe(200);
      invJson = (await invRes.json()) as typeof invJson;
      expect(invJson.incident_id).toBe("INC-HTTP-SSE-01");
      expect(invJson.current_phase).toBe("AWAITING_HITL_APPROVAL");

      // Read SSE stream chunks emitted during investigation
      while (!sseBuffer.includes("POST_MORTEM_READY")) {
        const next = await reader.read();
        if (next.done) break;
        sseBuffer += decoder.decode(next.value, { stream: true });
      }
      expect(sseBuffer).toContain("INCIDENT_STARTED");
      expect(sseBuffer).toContain("POST_MORTEM_READY");
      await reader.cancel();
    } finally {
      sseAbort.abort();
    }

    // 5. Approve pending HITL request via POST /api/v1/aegis/approvals/:id/approve
    const approvalId = invJson.pending_approvals[0]!.approval_id;
    const approveRes = await fetch(`${baseUrl}/api/v1/aegis/approvals/${approvalId}/approve`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        operator_id: "principal-sre-01",
        note: "Approved volatility protection guard during DB write spike"
      })
    });
    expect(approveRes.status).toBe(200);
    const approveJson = (await approveRes.json()) as { incident_phase: string };
    expect(approveJson.incident_phase).toBe("COMPLETED");

    // 6. Fetch Markdown Post-Mortem via GET /api/v1/aegis/incidents/:id/postmortem
    const pmRes = await fetch(`${baseUrl}/api/v1/aegis/incidents/INC-HTTP-SSE-01/postmortem`);
    expect(pmRes.status).toBe(200);
    const pmMarkdown = await pmRes.text();
    expect(pmMarkdown).toContain("flowchart TD");

    // 7. Rollback executed freeze via POST /api/v1/aegis/mitigations/rollback
    const freezeCmd = invJson.mitigation_outcomes.find((m) => m.status === "EXECUTED")?.signed_command;
    expect(freezeCmd).toBeDefined();
    const rbRes = await fetch(`${baseUrl}/api/v1/aegis/mitigations/rollback`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        rollback_token: freezeCmd!.rollback_token,
        reason: "HTTP E2E rollback verification"
      })
    });
    expect(rbRes.status).toBe(200);

    // 8. Check Prometheus /metrics & /api/v1/aegis/audit
    const metricsText = await (await fetch(`${baseUrl}/metrics`)).text();
    expect(metricsText).toContain("aegis_audit_chain_valid 1");

    const auditJson = (await (await fetch(`${baseUrl}/api/v1/aegis/audit`)).json()) as {
      integrity: { valid: boolean };
    };
    expect(auditJson.integrity.valid).toBe(true);
  });
});
