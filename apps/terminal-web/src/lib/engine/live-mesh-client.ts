/**
 * HYPERION Live Mesh Network Client (Dedicated WebWorker Only)
 * Connects directly to canonical Channel 01–08 endpoints:
 * - Gateway Binary WS: ws://localhost:8080/ws (Sec-WebSocket-Protocol: hyperion.bin.v1)
 * - Stream Aggregator REST: http://localhost:8083
 * - Sentinel-ML REST: http://localhost:8084
 * - Aegis SRE Agent REST/SSE: http://localhost:8085
 * - Chaos-Sim Control REST/SSE: http://localhost:8086
 * - Chronos-Replay REST/SSE: http://localhost:8088
 */

import { WS_CHANNEL_MASK, encodeClientControlFrame } from "../protocol/ws-binary-codec";
import type { ChaosRegime, CounterfactualForkConfig, InstrumentSymbol } from "../types";
import { INSTRUMENT_META } from "../types";

export interface LiveMeshEndpoints {
  gatewayWsUrl: string;
  gatewayHttpUrl: string;
  aggregatorHttpUrl: string;
  sentinelHttpUrl: string;
  aegisHttpUrl: string;
  chaosHttpUrl: string;
  chronosHttpUrl: string;
}

export const DEFAULT_ENDPOINTS: LiveMeshEndpoints = {
  gatewayWsUrl: "ws://127.0.0.1:8080/ws?format=binary",
  gatewayHttpUrl: "http://127.0.0.1:8080",
  aggregatorHttpUrl: "http://127.0.0.1:8083",
  sentinelHttpUrl: "http://127.0.0.1:8084",
  aegisHttpUrl: "http://127.0.0.1:8085",
  chaosHttpUrl: "http://127.0.0.1:8086",
  chronosHttpUrl: "http://127.0.0.1:8088",
};

export interface LiveMeshCallbacks {
  onBinaryFrame: (bytes: Uint8Array) => void;
  onJsonEvent: (source: string, data: unknown) => void;
  onWsStatusChange: (connected: boolean, reconnecting: boolean) => void;
  onServiceHealthUpdate: (health: Record<string, "ONLINE" | "SIMULATED" | "DEGRADED">) => void;
}

export class LiveMeshClient {
  private readonly endpoints: LiveMeshEndpoints;
  private readonly callbacks: LiveMeshCallbacks;
  private ws: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private healthTimer: ReturnType<typeof setInterval> | null = null;
  private reconnectAttempts = 0;
  private totalReconnects = 0;
  private desiredActive = false;
  private currentSymbol: InstrumentSymbol = "BTC-USDT";

  private aegisSse: EventSource | null = null;
  private chaosSse: EventSource | null = null;

  constructor(callbacks: LiveMeshCallbacks, endpoints: LiveMeshEndpoints = DEFAULT_ENDPOINTS) {
    this.callbacks = callbacks;
    this.endpoints = endpoints;
  }

  public get reconnectCount(): number {
    return this.totalReconnects;
  }

  public get isConnected(): boolean {
    return this.ws !== null && this.ws.readyState === WebSocket.OPEN;
  }

  public start(symbol: InstrumentSymbol): void {
    this.desiredActive = true;
    this.currentSymbol = symbol;
    this.connectGatewayWs();
    this.probeAllServiceHealth();
    if (!this.healthTimer) {
      this.healthTimer = setInterval(() => {
        if (this.desiredActive) {
          this.probeAllServiceHealth();
        }
      }, 6000);
    }
  }

  public stop(): void {
    this.desiredActive = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.healthTimer) {
      clearInterval(this.healthTimer);
      this.healthTimer = null;
    }
    if (this.ws) {
      this.ws.onopen = null;
      this.ws.onmessage = null;
      this.ws.onerror = null;
      this.ws.onclose = null;
      try {
        this.ws.close(1000, "Client stop");
      } catch {
        // Ignore close error
      }
      this.ws = null;
    }
    this.closeSseStreams();
  }

  private closeSseStreams(): void {
    if (this.aegisSse) {
      this.aegisSse.close();
      this.aegisSse = null;
    }
    if (this.chaosSse) {
      this.chaosSse.close();
      this.chaosSse = null;
    }
  }

  public updateSubscription(symbol: InstrumentSymbol): void {
    this.currentSymbol = symbol;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      const instBit = 1 << INSTRUMENT_META[symbol].id;
      const frame = encodeClientControlFrame(
        0x81,
        WS_CHANNEL_MASK.ALL_PUBLIC,
        instBit,
        0n,
        BigInt(Date.now()) * 1_000_000n
      );
      this.ws.send(frame);
    }
  }

  public requestSequenceResync(instrumentId: number, lastKnownSeq: bigint): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      const frame = encodeClientControlFrame(
        0x83,
        WS_CHANNEL_MASK.L2 | WS_CHANNEL_MASK.L3,
        1 << instrumentId,
        lastKnownSeq,
        BigInt(Date.now()) * 1_000_000n
      );
      this.ws.send(frame);
    }
  }

  /** Used by Resilience Verification & Playwright E2E test to force disconnect and test recovery. */
  public simulateForcedDisconnect(): void {
    this.totalReconnects += 1;
    if (this.ws) {
      try {
        this.ws.close(4001, "Simulated resilience fault");
      } catch {
        // Ignore
      }
      this.ws = null;
    }
    this.callbacks.onWsStatusChange(false, true);
    if (this.desiredActive) {
      this.scheduleReconnect(350);
    }
  }

  private connectGatewayWs(): void {
    if (!this.desiredActive || typeof WebSocket === "undefined") return;
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }

    try {
      const ws = new WebSocket(this.endpoints.gatewayWsUrl, ["hyperion.bin.v1"]);
      ws.binaryType = "arraybuffer";
      this.ws = ws;

      ws.onopen = () => {
        this.reconnectAttempts = 0;
        this.callbacks.onWsStatusChange(true, false);
        this.updateSubscription(this.currentSymbol);
        this.connectAuxiliarySse();
      };

      ws.onmessage = (ev: MessageEvent) => {
        if (ev.data instanceof ArrayBuffer) {
          this.callbacks.onBinaryFrame(new Uint8Array(ev.data));
        } else if (typeof ev.data === "string") {
          try {
            this.callbacks.onJsonEvent("gateway-ws", JSON.parse(ev.data));
          } catch {
            // Ignore non-JSON string keepalive
          }
        }
      };

      ws.onerror = () => {
        // Handled cleanly in onclose
      };

      ws.onclose = () => {
        this.ws = null;
        if (!this.desiredActive) return;
        this.callbacks.onWsStatusChange(false, true);
        const backoffMs = Math.min(5000, 400 * 2 ** Math.min(4, this.reconnectAttempts));
        this.reconnectAttempts += 1;
        this.totalReconnects += 1;
        this.scheduleReconnect(backoffMs);
      };
    } catch {
      this.callbacks.onWsStatusChange(false, true);
      this.scheduleReconnect(1500);
    }
  }

  private scheduleReconnect(delayMs: number): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
    }
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connectGatewayWs();
    }, delayMs);
  }

  private connectAuxiliarySse(): void {
    if (typeof EventSource === "undefined") return;
    this.closeSseStreams();
    try {
      this.aegisSse = new EventSource(`${this.endpoints.aegisHttpUrl}/events/incidents`);
      const handleAegisPayload = (raw: string) => {
        try {
          this.callbacks.onJsonEvent("aegis-sse", JSON.parse(raw));
        } catch {
          // Ignore malformed frame
        }
      };
      this.aegisSse.onmessage = (e) => handleAegisPayload(e.data);
      const namedAegisEvents = [
        "INCIDENT_STARTED",
        "PHASE_TRANSITION",
        "MCP_TOOL_INVOKED",
        "INJECTION_QUARANTINED",
        "POLICY_VERDICT",
        "HITL_APPROVAL_REQUIRED",
        "HITL_DECISION_RECORDED",
        "MITIGATION_EXECUTED",
        "MITIGATION_ROLLED_BACK",
        "POST_MORTEM_READY",
      ];
      for (const evtName of namedAegisEvents) {
        this.aegisSse.addEventListener(evtName, (e) => {
          handleAegisPayload((e as MessageEvent<string>).data);
        });
      }
      this.aegisSse.onerror = () => {
        this.aegisSse?.close();
        this.aegisSse = null;
      };
    } catch {
      // Optional auxiliary stream
    }

    try {
      this.chaosSse = new EventSource(`${this.endpoints.chaosHttpUrl}/events/chaos`);
      this.chaosSse.onmessage = (e) => {
        try {
          this.callbacks.onJsonEvent("chaos-sse", JSON.parse(e.data));
        } catch {
          // Ignore malformed frame
        }
      };
      this.chaosSse.onerror = () => {
        this.chaosSse?.close();
        this.chaosSse = null;
      };
    } catch {
      // Optional auxiliary stream
    }
  }

  private async probeEndpoint(url: string): Promise<boolean> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 900);
    try {
      const res = await fetch(url, { signal: controller.signal, cache: "no-store" });
      return res.ok;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  }

  public async probeAllServiceHealth(): Promise<void> {
    const targets: Array<[string, string]> = [
      ["gateway-go", "http://127.0.0.1:8081/healthz"],
      ["ledger-rs", "http://127.0.0.1:8082/healthz"],
      ["stream-aggregator", `${this.endpoints.aggregatorHttpUrl}/healthz`],
      ["sentinel-ml", `${this.endpoints.sentinelHttpUrl}/healthz`],
      ["aegis-agent", `${this.endpoints.aegisHttpUrl}/healthz`],
      ["chaos-sim", `${this.endpoints.chaosHttpUrl}/healthz`],
      ["chronos-replay", `${this.endpoints.chronosHttpUrl}/healthz`],
    ];

    const results = await Promise.all(
      targets.map(async ([name, url]) => {
        const ok = await this.probeEndpoint(url);
        return [name, ok ? "ONLINE" : "SIMULATED"] as const;
      })
    );

    const healthMap: Record<string, "ONLINE" | "SIMULATED" | "DEGRADED"> = {};
    for (const [name, state] of results) {
      healthMap[name] = state;
    }
    this.callbacks.onServiceHealthUpdate(healthMap);
  }

  public async dispatchChaosCampaign(regime: ChaosRegime, symbol: InstrumentSymbol, seed: bigint): Promise<boolean> {
    try {
      const res = await fetch(`${this.endpoints.chaosHttpUrl}/api/v1/chaos/campaigns`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          regime,
          enabled: regime !== "CALM",
          seed: seed.toString(),
          target_symbol: symbol,
          duration_ms: 60_000,
        }),
      });
      return res.ok;
    } catch {
      return false;
    }
  }

  public async dispatchChronosFork(config: CounterfactualForkConfig): Promise<unknown | null> {
    try {
      const res = await fetch(`${this.endpoints.chronosHttpUrl}/api/v1/chronos/fork`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          fork_name: config.forkName,
          symbol: config.symbol,
          start_engine_seq: config.startEngineSeq.toString(),
          end_engine_seq: config.endEngineSeq.toString(),
          maker_fee_ppm_override: config.makerFeePpmOverride,
          taker_fee_ppm_override: config.takerFeePpmOverride,
          max_order_qty_fp_override: config.maxOrderQtyFpOverride?.toString() ?? null,
          blocked_account_ids: config.blockedAccountIds.map((id) => id.toString()),
        }),
      });
      if (!res.ok) return null;
      return await res.json();
    } catch {
      return null;
    }
  }

  public async dispatchAegisApproval(
    commandId: string,
    decision: "APPROVE" | "REJECT" | "ROLLBACK",
    rollbackToken?: string
  ): Promise<boolean> {
    try {
      const url =
        decision === "ROLLBACK"
          ? `${this.endpoints.aegisHttpUrl}/api/v1/aegis/mitigations/rollback`
          : `${this.endpoints.aegisHttpUrl}/api/v1/aegis/approvals/${encodeURIComponent(commandId)}/${decision.toLowerCase()}`;
      const payload =
        decision === "ROLLBACK"
          ? {
              rollback_token: rollbackToken ?? commandId,
              reason: "Operator-initiated rollback dispatched from HYPERION Terminal",
            }
          : {
              operator_id: "terminal-web-operator",
              note: `Dispatched from HYPERION Terminal (${decision})`,
            };
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      return res.ok;
    } catch {
      return false;
    }
  }
}
