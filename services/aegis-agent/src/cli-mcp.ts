#!/usr/bin/env node
/**
 * HYPERION Channel 06 — Dedicated Lightweight Stdio MCP Server Entrypoint for IDE / CLI Integration.
 *
 * Avoids eagerly loading HTTP/Express or LLM SDKs so IDE/CLI cold startup is sub-200ms.
 */

import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  DeterministicSimulationBackend,
  ResilientHttpUpstreamBackend
} from "./adapters/upstream-clients.js";
import { loadAegisConfig } from "./config.js";
import { ImmutableAuditChain } from "./crypto/audit-chain.js";
import { AegisCryptographicSigner } from "./crypto/signer.js";
import { SystemNanosecondClock } from "./domain/clock.js";
import { PromptInjectionFirewall } from "./guardrails/injection-firewall.js";
import { SafetyPolicyEngine } from "./guardrails/policy-engine.js";
import { createAegisMcpServer } from "./mcp/server.js";

async function main(): Promise<void> {
  const config = loadAegisConfig({
    ...process.env,
    AEGIS_TRANSPORT: "stdio"
  });
  const clock = new SystemNanosecondClock();
  const signer = new AegisCryptographicSigner({
    seedHex: config.ed25519SeedHex,
    hmacSecret: config.hmacSecret
  });
  const auditChain = new ImmutableAuditChain(clock, signer);
  const firewall = new PromptInjectionFirewall();
  const simulationBackend = new DeterministicSimulationBackend(clock);
  const upstream =
    config.upstreamMode === "simulation"
      ? simulationBackend
      : new ResilientHttpUpstreamBackend(config, simulationBackend);

  const policyEngine = new SafetyPolicyEngine({
    clock,
    signer,
    auditChain,
    firewall,
    sink: upstream
  });

  const mcpServer = createAegisMcpServer({
    clock,
    upstream,
    policyEngine,
    auditChain,
    firewall,
    incidentStore: {
      getIncident: () => undefined,
      listIncidents: () => []
    }
  });

  const stdioTransport = new StdioServerTransport();
  await mcpServer.connect(stdioTransport);
  console.error(
    "[aegis-mcp-server] Connected via stdio transport (strict Zod + Safety Policy Gate active)"
  );
}

main().catch((err: unknown) => {
  console.error("[aegis-mcp-server] Fatal stdio startup error:", err);
  process.exit(1);
});
