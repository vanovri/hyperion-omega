#!/usr/bin/env node
/**
 * HYPERION Channel 06 — Aegis Autonomous Incident-Response & Financial Risk Co-Pilot.
 *
 * Public module exports + dual-transport runtime factory (`stdio` & `Streamable HTTP / SSE :8085`).
 */

import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import type { Server as HttpServer } from "node:http";
import { pathToFileURL } from "node:url";
import {
  DeterministicRuleReasoner,
  ResilientDualReasoner,
  type AegisReasoner
} from "./adapters/llm-reasoner.js";
import {
  DeterministicSimulationBackend,
  ResilientHttpUpstreamBackend,
  type HyperionUpstreamBackend
} from "./adapters/upstream-clients.js";
import { loadAegisConfig, type AegisRuntimeConfig } from "./config.js";
import { ImmutableAuditChain } from "./crypto/audit-chain.js";
import {
  buildDeterministicMerkleTree,
  computeAccountLeafBlake3,
  computeMerkleParentBlake3,
  verifyMerkleLedgerProof
} from "./crypto/merkle.js";
import { AegisCryptographicSigner } from "./crypto/signer.js";
import {
  DeterministicNanosecondClock,
  SystemNanosecondClock,
  formatEpochNsIso,
  type NanosecondClock
} from "./domain/clock.js";
import * as fixedPoint from "./domain/fixed-point.js";
import * as schemas from "./domain/schemas.js";
import type { SafetyPolicyConfig } from "./domain/types.js";
import { generateIncidentPostMortemMarkdown } from "./engine/post-mortem.js";
import { AutonomousStateGraphEngine } from "./engine/state-graph.js";
import { PromptInjectionFirewall } from "./guardrails/injection-firewall.js";
import { SafetyPolicyEngine } from "./guardrails/policy-engine.js";
import { createAegisMcpServer } from "./mcp/server.js";
import { createAegisHttpApp } from "./server/http-app.js";

export {
  AegisCryptographicSigner,
  AutonomousStateGraphEngine,
  DeterministicNanosecondClock,
  DeterministicRuleReasoner,
  DeterministicSimulationBackend,
  ImmutableAuditChain,
  PromptInjectionFirewall,
  ResilientDualReasoner,
  ResilientHttpUpstreamBackend,
  SafetyPolicyEngine,
  SystemNanosecondClock,
  buildDeterministicMerkleTree,
  computeAccountLeafBlake3,
  computeMerkleParentBlake3,
  createAegisHttpApp,
  createAegisMcpServer,
  fixedPoint,
  formatEpochNsIso,
  generateIncidentPostMortemMarkdown,
  loadAegisConfig,
  schemas,
  verifyMerkleLedgerProof
};

export type * from "./domain/types.js";

export interface CreateAegisRuntimeOptions {
  env?: NodeJS.ProcessEnv;
  clock?: NanosecondClock;
  policyOverrides?: Partial<SafetyPolicyConfig>;
  offlineSimulationOnly?: boolean;
  upstreamOverride?: HyperionUpstreamBackend;
  reasonerOverride?: AegisReasoner;
}

export interface AegisRuntime {
  config: AegisRuntimeConfig;
  clock: NanosecondClock;
  signer: AegisCryptographicSigner;
  auditChain: ImmutableAuditChain;
  firewall: PromptInjectionFirewall;
  simulationBackend: DeterministicSimulationBackend;
  upstream: HyperionUpstreamBackend;
  policyEngine: SafetyPolicyEngine;
  reasoner: AegisReasoner;
  engine: AutonomousStateGraphEngine;
  createMcpServerInstance: () => ReturnType<typeof createAegisMcpServer>;
  startHttpServer: (portOverride?: number) => Promise<{ server: HttpServer; port: number }>;
}

export function createAegisRuntime(options?: CreateAegisRuntimeOptions): AegisRuntime {
  const config = loadAegisConfig(options?.env);
  const clock = options?.clock ?? new SystemNanosecondClock();
  const signer = new AegisCryptographicSigner({
    seedHex: config.ed25519SeedHex,
    hmacSecret: config.hmacSecret
  });
  const auditChain = new ImmutableAuditChain(clock, signer);
  const firewall = new PromptInjectionFirewall();
  const simulationBackend =
    options?.upstreamOverride instanceof DeterministicSimulationBackend
      ? options.upstreamOverride
      : new DeterministicSimulationBackend(clock);
  const useSimulationDirectly =
    options?.offlineSimulationOnly === true || config.upstreamMode === "simulation";
  const upstream =
    options?.upstreamOverride ??
    (useSimulationDirectly
      ? simulationBackend
      : new ResilientHttpUpstreamBackend(config, simulationBackend));

  const engineHolder: { current?: AutonomousStateGraphEngine } = {};

  const policyEngine = new SafetyPolicyEngine({
    config: options?.policyOverrides,
    clock,
    signer,
    auditChain,
    firewall,
    sink: upstream,
    onEvent: (eventType, incidentId, payload) => {
      engineHolder.current?.emitStreamEvent(eventType, incidentId, payload);
    }
  });

  const reasoner = options?.reasonerOverride ?? new ResilientDualReasoner(config);
  const engine = new AutonomousStateGraphEngine({
    clock,
    upstream,
    policyEngine,
    auditChain,
    firewall,
    reasoner
  });
  engineHolder.current = engine;

  const createMcpServerInstance = () =>
    createAegisMcpServer(
      {
        clock,
        upstream,
        policyEngine,
        auditChain,
        firewall,
        incidentStore: engine
      },
      engine.getToolOperations()
    );

  const startHttpServer = async (
    portOverride?: number
  ): Promise<{ server: HttpServer; port: number }> => {
    const app = createAegisHttpApp({
      config,
      clock,
      signer,
      auditChain,
      firewall,
      policyEngine,
      upstream,
      engine
    });

    const sweepTimer = setInterval(() => {
      void policyEngine.processDueAutoRollbacks();
    }, 1_000);
    sweepTimer.unref();

    const targetPort = portOverride ?? config.httpPort;
    return await new Promise<{ server: HttpServer; port: number }>((resolve, reject) => {
      const server = app.listen(targetPort, "0.0.0.0", () => {
        const addr = server.address();
        const boundPort = typeof addr === "object" && addr ? addr.port : targetPort;
        server.on("close", () => clearInterval(sweepTimer));
        resolve({ server, port: boundPort });
      });
      server.on("error", reject);
    });
  };

  return {
    config,
    clock,
    signer,
    auditChain,
    firewall,
    simulationBackend,
    upstream,
    policyEngine,
    reasoner,
    engine,
    createMcpServerInstance,
    startHttpServer
  };
}

async function runCliEntrypoint(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log(
      [
        "HYPERION Channel 06 — Aegis Autonomous SRE & MCP Co-Pilot",
        "Usage:",
        "  node dist/index.js             Start HTTP/SSE + Streamable HTTP MCP server (:8085)",
        "  node dist/index.js --stdio     Start stdio MCP server for IDE/CLI integration",
        "  AEGIS_TRANSPORT=stdio ...      Environment toggle for stdio MCP transport"
      ].join("\n")
    );
    return;
  }

  const useStdio = args.includes("--stdio") || process.env.AEGIS_TRANSPORT === "stdio";
  const runtime = createAegisRuntime();

  if (useStdio) {
    const mcpServer = runtime.createMcpServerInstance();
    const transport = new StdioServerTransport();
    await mcpServer.connect(transport);
    console.error("[aegis-agent] MCP server active on stdio");
    return;
  }

  const { port } = await runtime.startHttpServer();
  console.log(
    `[aegis-agent] Listening on http://0.0.0.0:${port} (MCP=/mcp, SSE=/events/incidents, REST=/api/v1/aegis/*)`
  );
}

const isDirectExecution =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isDirectExecution) {
  runCliEntrypoint().catch((err: unknown) => {
    console.error("[aegis-agent] Fatal startup failure:", err);
    process.exit(1);
  });
}
