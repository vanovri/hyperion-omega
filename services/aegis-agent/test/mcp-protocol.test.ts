import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  DeterministicNanosecondClock,
  createAegisRuntime
} from "../src/index.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.resolve(__dirname, "..");

describe("Production Custom MCP Server Protocol Compliance (InMemory & Stdio)", () => {
  it("exposes Zod-validated Tools, Resources, and Prompts and handles dry-run + TTL auto-rollback", async () => {
    const clock = new DeterministicNanosecondClock();
    const localRuntime = createAegisRuntime({
      clock,
      offlineSimulationOnly: true
    });

    const server = localRuntime.createMcpServerInstance();
    const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
    const client = new Client({ name: "mcp-compliance-suite", version: "1.0.0" });

    await server.connect(serverTransport);
    await client.connect(clientTransport);

    try {
      // 1. Verify all 4 MCP Tools are registered with descriptions & schemas
      const { tools } = await client.listTools();
      const toolNames = tools.map((t) => t.name).sort();
      expect(toolNames).toEqual([
        "analyze_system_telemetry",
        "execute_risk_mitigation",
        "inspect_fraud_subgraph",
        "query_ledger_merkle_audit"
      ]);

      // 2. Verify Resources & Prompts discovery
      const { resources } = await client.listResources();
      expect(resources.map((r) => r.uri)).toEqual(
        expect.arrayContaining([
          "aegis://policies/active",
          "aegis://audit/chain",
          "aegis://approvals/pending"
        ])
      );

      const { prompts } = await client.listPrompts();
      expect(prompts.map((p) => p.name).sort()).toEqual([
        "audit_account_solvency",
        "investigate_market_incident",
        "review_hitl_mitigation"
      ]);

      const promptMsg = await client.getPrompt({
        name: "investigate_market_incident",
        arguments: {
          alert_id: "alert-mcp-01",
          suspect_account_id: "9001",
          symbol: "BTC-USDT"
        }
      });
      expect(promptMsg.messages[0]?.content.type).toBe("text");

      // 3. Call query_ledger_merkle_audit (both markdown & json)
      const merkleRes = await client.callTool({
        name: "query_ledger_merkle_audit",
        arguments: {
          account_id: "9001",
          asset: "USDT",
          response_format: "markdown"
        }
      });
      expect(merkleRes.isError).toBeFalsy();
      expect((merkleRes.structuredContent as { verdict: string }).verdict).toBe("VERIFIED_SOLVENT");

      // 4. Call inspect_fraud_subgraph -> populates verified forensic evidence in PolicyEngine
      const subRes = await client.callTool({
        name: "inspect_fraud_subgraph",
        arguments: {
          center_account_id: "9001",
          symbol: "BTC-USDT",
          max_hops: 3,
          response_format: "json"
        }
      });
      expect(subRes.isError).toBeFalsy();
      const subStructured = subRes.structuredContent as {
        security_taint_report: { verified_offender_account_ids: string[] };
      };
      expect(subStructured.security_taint_report.verified_offender_account_ids).toEqual([
        "9001",
        "9002",
        "9003"
      ]);

      // 5. Call analyze_system_telemetry
      const telRes = await client.callTool({
        name: "analyze_system_telemetry",
        arguments: {
          include_redpanda_lag: true,
          include_circuit_breakers: true,
          window_seconds: 30
        }
      });
      expect((telRes.structuredContent as { overall_health: string }).overall_health).toBe("HEALTHY");

      // 6. Dry-run execute_risk_mitigation -> zero state mutation in dispatchedMitigations
      const dryMit = await client.callTool({
        name: "execute_risk_mitigation",
        arguments: {
          incident_id: "INC-MCP-001",
          source_alert_id: "alert-wash-default-01",
          action: "THROTTLE_RATE_LIMIT",
          target_account_ids: ["9001"],
          target_symbol: "BTC-USDT",
          rate_limit_override_rps: 5,
          cooldown_duration_ms: 10_000,
          dry_run: true,
          reason: "Dry-run rate-limit simulation on verified syndicate account 9001"
        }
      });
      expect((dryMit.structuredContent as { status: string }).status).toBe("DRY_RUN_SIMULATED");
      expect(localRuntime.simulationBackend.dispatchedMitigations.length).toBe(0);

      // 7. Live execute_risk_mitigation -> dispatches signed Protobuf command + auto-rolls back after TTL
      const liveMit = await client.callTool({
        name: "execute_risk_mitigation",
        arguments: {
          incident_id: "INC-MCP-001",
          source_alert_id: "alert-wash-default-01",
          action: "THROTTLE_RATE_LIMIT",
          target_account_ids: ["9001"],
          target_symbol: "BTC-USDT",
          rate_limit_override_rps: 5,
          cooldown_duration_ms: 10_000,
          dry_run: false,
          reason: "Live rate-limit enforcement on verified syndicate account 9001"
        }
      });
      expect((liveMit.structuredContent as { status: string }).status).toBe("EXECUTED");
      expect(localRuntime.simulationBackend.dispatchedMitigations.length).toBe(1);
      expect(localRuntime.policyEngine.getActiveMitigations().length).toBe(1);

      // Advance deterministic clock past 10_000ms TTL and trigger auto-rollback sweep
      clock.advanceMs(10_500);
      const rolledBack = await localRuntime.policyEngine.processDueAutoRollbacks();
      expect(rolledBack.length).toBe(1);
      expect(rolledBack[0]?.trigger).toBe("AUTO_TTL_EXPIRY");
      expect(localRuntime.policyEngine.getActiveMitigations().length).toBe(0);
      expect(localRuntime.simulationBackend.dispatchedMitigations.at(-1)?.action).toBe("RESTORE_NORMAL");

      // 8. Read audit chain & policy resources and verify integrity == true
      const auditResource = await client.readResource({ uri: "aegis://audit/chain" });
      const firstContent = auditResource.contents[0];
      const auditJsonText = firstContent && "text" in firstContent ? firstContent.text : "{}";
      const auditBody = JSON.parse(auditJsonText) as {
        integrity: { valid: boolean };
      };
      expect(auditBody.integrity.valid).toBe(true);

      const policyResource = await client.readResource({ uri: "aegis://policies/active" });
      const policyContent = policyResource.contents[0];
      const policyText = policyContent && "text" in policyContent ? policyContent.text : "{}";
      expect(JSON.parse(policyText)).toMatchObject({
        policy_version: "hyperion-aegis-policy-v1.0",
        protected_system_account_ids: ["1", "2"]
      });

      // 9. Strict Zod schema validation rejects malformed / floating-point account IDs
      const invalidCall = await client.callTool({
        name: "query_ledger_merkle_audit",
        arguments: {
          account_id: "9001.55",
          asset: "USDT"
        }
      });
      expect(invalidCall.isError).toBe(true);
      const invalidText =
        Array.isArray(invalidCall.content) &&
        invalidCall.content[0] &&
        "text" in invalidCall.content[0]
          ? String(invalidCall.content[0].text)
          : "";
      expect(invalidText).toContain("Must be an unsigned 64-bit decimal integer string");
    } finally {
      await client.close();
      await server.close();
    }
  });

  it(
    "connects cleanly over real subprocess stdio transport (compiled dist/cli-mcp.js)",
    async () => {
      const compiledEntry = path.join(PROJECT_ROOT, "dist", "cli-mcp.js");
      const useCompiled = existsSync(compiledEntry);
      const tsxCli = path.join(PROJECT_ROOT, "node_modules", "tsx", "dist", "cli.mjs");
      const tsEntry = path.join(PROJECT_ROOT, "src", "cli-mcp.ts");

      const transport = new StdioClientTransport({
        command: process.execPath,
        args: useCompiled ? [compiledEntry] : [tsxCli, tsEntry],
        cwd: PROJECT_ROOT,
        env: {
          AEGIS_TRANSPORT: "stdio",
          AEGIS_UPSTREAM_MODE: "simulation"
        },
        stderr: "pipe"
      });
      const client = new Client({ name: "stdio-e2e-verifier", version: "1.0.0" });

      await client.connect(transport);
      try {
        const { tools } = await client.listTools();
        expect(tools.length).toBe(4);
        const auditRes = await client.callTool({
          name: "query_ledger_merkle_audit",
          arguments: {
            account_id: "1001",
            asset: "USDT"
          }
        });
        expect((auditRes.structuredContent as { verdict: string }).verdict).toBe("VERIFIED_SOLVENT");
      } finally {
        await client.close();
      }
    },
    60_000
  );
});
