/**
 * HYPERION Channel 06 — Express Control Plane, Streamable HTTP MCP Endpoint & Live SSE Stream (`:8085`).
 *
 * Exposes canonical endpoints defined in `00-hyperion-contract.md` §3.1:
 * - `GET  /healthz`                                    (HealthStatusResponse)
 * - `GET  /metrics`                                    (Prometheus OpenMetrics text)
 * - `POST|GET|DELETE /mcp`                             (MCP Streamable HTTP / SSE transport)
 * - `GET  /events/incidents` & `/api/v1/aegis/events`  (Live SRE Investigation & HITL SSE Stream)
 * - `POST /api/v1/aegis/investigations`                (Trigger autonomous investigation)
 * - `GET  /api/v1/aegis/incidents`                     (List incidents)
 * - `GET  /api/v1/aegis/incidents/:id`                 (Get incident record)
 * - `GET  /api/v1/aegis/incidents/:id/postmortem`      (Get Markdown post-mortem)
 * - `GET  /api/v1/aegis/approvals`                     (List pending/all HITL approvals)
 * - `POST /api/v1/aegis/approvals/:id/approve`         (Operator HITL approve + execute)
 * - `POST /api/v1/aegis/approvals/:id/reject`          (Operator HITL reject)
 * - `POST /api/v1/aegis/mitigations/rollback`          (Execute cryptographic rbk1.* rollback)
 * - `GET  /api/v1/aegis/audit`                         (Immutable BLAKE3 + Ed25519 audit chain)
 * - `GET  /api/v1/aegis/policies`                      (Active SafetyPolicyConfig)
 */

import { timingSafeEqual } from "node:crypto";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import cors from "cors";
import express, { type Express, type NextFunction, type Request, type Response } from "express";
import { ZodError } from "zod";
import type { AegisRuntimeConfig } from "../config.js";
import type { HyperionUpstreamBackend } from "../adapters/upstream-clients.js";
import type { ImmutableAuditChain } from "../crypto/audit-chain.js";
import type { AegisCryptographicSigner } from "../crypto/signer.js";
import type { NanosecondClock } from "../domain/clock.js";
import {
  HitlDecisionRequestSchema,
  RollbackRequestSchema,
  TriggerInvestigationRequestSchema
} from "../domain/schemas.js";
import type { AutonomousStateGraphEngine } from "../engine/state-graph.js";
import type { PromptInjectionFirewall } from "../guardrails/injection-firewall.js";
import type { SafetyPolicyEngine } from "../guardrails/policy-engine.js";
import { createAegisMcpServer } from "../mcp/server.js";

export interface AegisHttpDependencies {
  config: AegisRuntimeConfig;
  clock: NanosecondClock;
  signer: AegisCryptographicSigner;
  auditChain: ImmutableAuditChain;
  firewall: PromptInjectionFirewall;
  policyEngine: SafetyPolicyEngine;
  upstream: HyperionUpstreamBackend;
  engine: AutonomousStateGraphEngine;
}

function sendErrorEnvelope(res: Response, status: number, code: string, message: string, tsNs: bigint): void {
  res.status(status).json({
    code,
    message,
    trace_id: `aegis-${tsNs.toString(16).padStart(16, "0")}`,
    ts_ns: tsNs.toString(10)
  });
}

function constantTimeEquals(a: string, b: string): boolean {
  const bufA = Buffer.from(a, "utf8");
  const bufB = Buffer.from(b, "utf8");
  if (bufA.length !== bufB.length) {
    return false;
  }
  return timingSafeEqual(bufA, bufB);
}

export function createAegisHttpApp(deps: AegisHttpDependencies): Express {
  const app = express();
  app.disable("x-powered-by");
  const allowedOrigins = new Set(deps.config.corsOrigins);
  app.use(
    cors({
      origin: (origin, callback) => {
        if (!origin || allowedOrigins.has("*") || allowedOrigins.has(origin)) {
          callback(null, true);
          return;
        }
        callback(null, false);
      }
    })
  );
  app.use(express.json({ limit: "2mb" }));

  // Internal zero-trust mutation guard (`SEC-04`): enforces X-Hyperion-Internal-Token
  // on all mutating control-plane & MCP routes whenever configured (mandatory in production).
  const requireInternalMutationAuth = (req: Request, res: Response, next: NextFunction): void => {
    const expected = deps.config.internalAuthToken;
    if (!expected) {
      next();
      return;
    }
    const provided = req.header("x-hyperion-internal-token") ?? "";
    if (!provided || !constantTimeEquals(provided, expected)) {
      sendErrorEnvelope(
        res,
        401,
        "UNAUTHORIZED_INTERNAL_ENDPOINT",
        "Missing or invalid X-Hyperion-Internal-Token header for Aegis mutation endpoint.",
        deps.clock.nowNs()
      );
      return;
    }
    next();
  };

  app.use((req: Request, res: Response, next: NextFunction) => {
    if (req.method === "POST" || req.method === "PUT" || req.method === "DELETE" || req.method === "PATCH") {
      requireInternalMutationAuth(req, res, next);
      return;
    }
    next();
  });

  // --------------------------------------------------------------------------
  // 1. Liveness & Readiness (`/healthz`)
  // --------------------------------------------------------------------------
  app.get("/healthz", (_req: Request, res: Response) => {
    const chainCheck = deps.auditChain.verifyChain();
    res.status(chainCheck.valid ? 200 : 503).json({
      status: chainCheck.valid ? "ok" : "degraded",
      service: deps.config.serviceName,
      version: deps.config.serviceVersion,
      audit_chain_entries: deps.auditChain.getLength(),
      audit_chain_valid: chainCheck.valid,
      ed25519_public_key_hex: deps.signer.getPublicKeyHex(),
      ts_ns: deps.clock.nowNs().toString(10)
    });
  });

  // --------------------------------------------------------------------------
  // 2. Prometheus Metrics (`/metrics`)
  // --------------------------------------------------------------------------
  app.get("/metrics", (_req: Request, res: Response) => {
    const incidents = deps.engine.listIncidents();
    const auditEntries = deps.auditChain.getEntries();
    const chainStatus = deps.auditChain.verifyChain();
    const executedCount = auditEntries.filter((e) => e.event_type === "MITIGATION_EXECUTED").length;
    const blockedInjectionCount = auditEntries.filter(
      (e) =>
        e.event_type === "MITIGATION_BLOCKED_PROMPT_INJECTION" ||
        e.event_type === "PROMPT_INJECTION_METADATA_QUARANTINED"
    ).length;
    const blockedPolicyCount = auditEntries.filter(
      (e) => e.event_type === "MITIGATION_BLOCKED_POLICY"
    ).length;
    const pendingHitlCount = deps.policyEngine.getPendingApprovals().length;

    const lines = [
      "# HELP aegis_incidents_total Total autonomous investigations processed by Aegis.",
      "# TYPE aegis_incidents_total counter",
      `aegis_incidents_total ${incidents.length}`,
      "# HELP aegis_mitigations_executed_total Total cryptographically signed mitigations executed.",
      "# TYPE aegis_mitigations_executed_total counter",
      `aegis_mitigations_executed_total ${executedCount}`,
      "# HELP aegis_prompt_injections_blocked_total Total prompt-injection attempts quarantined or blocked.",
      "# TYPE aegis_prompt_injections_blocked_total counter",
      `aegis_prompt_injections_blocked_total ${blockedInjectionCount}`,
      "# HELP aegis_policy_blocks_total Total unverified/illegal actions blocked by Policy Gate.",
      "# TYPE aegis_policy_blocks_total counter",
      `aegis_policy_blocks_total ${blockedPolicyCount}`,
      "# HELP aegis_hitl_pending_approvals Current pending Human-in-the-Loop approval requests.",
      "# TYPE aegis_hitl_pending_approvals gauge",
      `aegis_hitl_pending_approvals ${pendingHitlCount}`,
      "# HELP aegis_audit_chain_entries_total Total entries in the immutable BLAKE3 audit chain.",
      "# TYPE aegis_audit_chain_entries_total counter",
      `aegis_audit_chain_entries_total ${auditEntries.length}`,
      "# HELP aegis_audit_chain_valid 1 if the BLAKE3 + Ed25519 audit chain passes full verification, 0 otherwise.",
      "# TYPE aegis_audit_chain_valid gauge",
      `aegis_audit_chain_valid ${chainStatus.valid ? 1 : 0}`,
      ""
    ];

    res.setHeader("Content-Type", "text/plain; version=0.0.4; charset=utf-8");
    res.status(200).send(lines.join("\n"));
  });

  // --------------------------------------------------------------------------
  // 3. Streamable HTTP MCP Endpoint (`/mcp`)
  // --------------------------------------------------------------------------
  app.post("/mcp", async (req: Request, res: Response) => {
    const mcpServer = createAegisMcpServer(
      {
        clock: deps.clock,
        upstream: deps.upstream,
        policyEngine: deps.policyEngine,
        auditChain: deps.auditChain,
        firewall: deps.firewall,
        incidentStore: deps.engine
      },
      deps.engine.getToolOperations()
    );
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: undefined,
      enableJsonResponse: true
    });

    res.on("close", () => {
      void transport.close();
      void mcpServer.close();
    });

    try {
      await mcpServer.connect(transport);
      await transport.handleRequest(req, res, req.body);
    } catch (err) {
      if (!res.headersSent) {
        sendErrorEnvelope(
          res,
          500,
          "MCP_TRANSPORT_ERROR",
          err instanceof Error ? err.message : String(err),
          deps.clock.nowNs()
        );
      }
    }
  });

  app.get("/mcp", (_req: Request, res: Response) => {
    res.status(405).json({
      jsonrpc: "2.0",
      error: {
        code: -32000,
        message: "Stateless Streamable HTTP MCP endpoint expects POST /mcp JSON-RPC requests; subscribe to GET /events/incidents for live SRE SSE telemetry."
      },
      id: null
    });
  });

  app.delete("/mcp", (_req: Request, res: Response) => {
    res.status(204).end();
  });

  // --------------------------------------------------------------------------
  // 4. Live Investigation & HITL SSE Stream (`/events/incidents` & `/api/v1/aegis/events`)
  // --------------------------------------------------------------------------
  const sseHandler = (req: Request, res: Response): void => {
    res.setHeader("Content-Type", "text/event-stream; charset=utf-8");
    res.setHeader("Cache-Control", "no-cache, no-transform");
    res.setHeader("Connection", "keep-alive");
    res.setHeader("X-Accel-Buffering", "no");
    res.flushHeaders();

    const connectedPayload = JSON.stringify({
      event_type: "SSE_STREAM_CONNECTED",
      service: deps.config.serviceName,
      pending_approvals: deps.policyEngine.getPendingApprovals().length,
      ts_ns: deps.clock.nowNs().toString(10)
    });
    res.write(`event: connected\ndata: ${connectedPayload}\n\n`);

    const unsubscribe = deps.engine.subscribeEvents((evt) => {
      res.write(`id: ${evt.event_id}\nevent: ${evt.event_type}\ndata: ${JSON.stringify(evt)}\n\n`);
    });

    const heartbeat = setInterval(() => {
      res.write(`: heartbeat ts_ns=${deps.clock.nowNs().toString(10)}\n\n`);
    }, 15_000);
    heartbeat.unref();

    req.on("close", () => {
      clearInterval(heartbeat);
      unsubscribe();
    });
  };

  app.get("/events/incidents", sseHandler);
  app.get("/api/v1/aegis/events", sseHandler);

  // --------------------------------------------------------------------------
  // 5. Autonomous Investigation & Post-Mortem REST API (`/api/v1/aegis/*`)
  // --------------------------------------------------------------------------
  app.post("/api/v1/aegis/investigations", async (req: Request, res: Response) => {
    try {
      const parsed = TriggerInvestigationRequestSchema.parse(req.body);
      const record = await deps.engine.runInvestigation(parsed.alert, parsed.incident_id);
      res.status(200).json(record);
    } catch (err) {
      if (err instanceof ZodError) {
        sendErrorEnvelope(res, 400, "VALIDATION_ERROR", err.message, deps.clock.nowNs());
        return;
      }
      sendErrorEnvelope(
        res,
        500,
        "INVESTIGATION_ERROR",
        err instanceof Error ? err.message : String(err),
        deps.clock.nowNs()
      );
    }
  });

  app.get("/api/v1/aegis/incidents", (_req: Request, res: Response) => {
    res.status(200).json({
      incidents: deps.engine.listIncidents(),
      ts_ns: deps.clock.nowNs().toString(10)
    });
  });

  app.get("/api/v1/aegis/incidents/:incidentId", (req: Request, res: Response) => {
    const incidentId = String(req.params.incidentId ?? "");
    const record = deps.engine.getIncident(incidentId);
    if (!record) {
      sendErrorEnvelope(
        res,
        404,
        "INCIDENT_NOT_FOUND",
        `Incident ${incidentId} not found`,
        deps.clock.nowNs()
      );
      return;
    }
    res.status(200).json(record);
  });

  app.get("/api/v1/aegis/incidents/:incidentId/postmortem", (req: Request, res: Response) => {
    const incidentId = String(req.params.incidentId ?? "");
    const record = deps.engine.getIncident(incidentId);
    if (!record || !record.post_mortem_markdown) {
      sendErrorEnvelope(
        res,
        404,
        "POSTMORTEM_NOT_FOUND",
        `Post-mortem not available for incident ${incidentId}`,
        deps.clock.nowNs()
      );
      return;
    }
    const format = String(req.query.format ?? "markdown").toLowerCase();
    if (format === "json") {
      res.status(200).json({
        incident_id: record.incident_id,
        phase: record.current_phase,
        markdown: record.post_mortem_markdown,
        ts_ns: deps.clock.nowNs().toString(10)
      });
      return;
    }
    res.setHeader("Content-Type", "text/markdown; charset=utf-8");
    res.status(200).send(record.post_mortem_markdown);
  });

  // --------------------------------------------------------------------------
  // 6. Human-in-the-Loop (HITL) Approval Queue & Rollback API
  // --------------------------------------------------------------------------
  app.get("/api/v1/aegis/approvals", (req: Request, res: Response) => {
    const statusFilter = String(req.query.status ?? "pending").toLowerCase();
    const items =
      statusFilter === "all"
        ? deps.policyEngine.getAllApprovals()
        : deps.policyEngine.getPendingApprovals();
    res.status(200).json({
      count: items.length,
      approvals: items,
      ts_ns: deps.clock.nowNs().toString(10)
    });
  });

  app.post("/api/v1/aegis/approvals/:approvalId/approve", async (req: Request, res: Response) => {
    try {
      const approvalId = String(req.params.approvalId ?? "");
      const body = HitlDecisionRequestSchema.parse(req.body);
      const { approval, execution } = await deps.policyEngine.approveHitlRequest({
        approvalId,
        operatorId: body.operator_id,
        note: body.note
      });
      const updatedIncident = deps.engine.refreshIncidentAfterHitlDecision(
        approval.incident_id,
        execution
      );
      res.status(200).json({
        approval,
        execution,
        incident_phase: updatedIncident?.current_phase ?? "COMPLETED"
      });
    } catch (err) {
      if (err instanceof ZodError) {
        sendErrorEnvelope(res, 400, "VALIDATION_ERROR", err.message, deps.clock.nowNs());
        return;
      }
      sendErrorEnvelope(
        res,
        400,
        "HITL_APPROVAL_FAILED",
        err instanceof Error ? err.message : String(err),
        deps.clock.nowNs()
      );
    }
  });

  app.post("/api/v1/aegis/approvals/:approvalId/reject", (req: Request, res: Response) => {
    try {
      const approvalId = String(req.params.approvalId ?? "");
      const body = HitlDecisionRequestSchema.parse(req.body);
      const approval = deps.policyEngine.rejectHitlRequest({
        approvalId,
        operatorId: body.operator_id,
        note: body.note
      });
      const updatedIncident = deps.engine.refreshIncidentAfterHitlDecision(approval.incident_id);
      res.status(200).json({
        approval,
        incident_phase: updatedIncident?.current_phase ?? "COMPLETED"
      });
    } catch (err) {
      if (err instanceof ZodError) {
        sendErrorEnvelope(res, 400, "VALIDATION_ERROR", err.message, deps.clock.nowNs());
        return;
      }
      sendErrorEnvelope(
        res,
        400,
        "HITL_REJECTION_FAILED",
        err instanceof Error ? err.message : String(err),
        deps.clock.nowNs()
      );
    }
  });

  app.post("/api/v1/aegis/mitigations/rollback", async (req: Request, res: Response) => {
    try {
      const body = RollbackRequestSchema.parse(req.body);
      const result = await deps.policyEngine.rollbackByToken(
        body.rollback_token,
        body.reason,
        "MANUAL_TOKEN"
      );
      res.status(200).json(result);
    } catch (err) {
      if (err instanceof ZodError) {
        sendErrorEnvelope(res, 400, "VALIDATION_ERROR", err.message, deps.clock.nowNs());
        return;
      }
      sendErrorEnvelope(
        res,
        400,
        "ROLLBACK_FAILED",
        err instanceof Error ? err.message : String(err),
        deps.clock.nowNs()
      );
    }
  });

  app.get("/api/v1/aegis/audit", (req: Request, res: Response) => {
    const incidentId = req.query.incident_id ? String(req.query.incident_id) : undefined;
    const integrity = deps.auditChain.verifyChain();
    res.status(200).json({
      integrity,
      entries: deps.auditChain.getEntries(incidentId),
      ts_ns: deps.clock.nowNs().toString(10)
    });
  });

  app.get("/api/v1/aegis/policies", (_req: Request, res: Response) => {
    res.status(200).json({
      policy: deps.policyEngine.getConfig(),
      active_mitigations: deps.policyEngine.getActiveMitigations(),
      ts_ns: deps.clock.nowNs().toString(10)
    });
  });

  return app;
}
