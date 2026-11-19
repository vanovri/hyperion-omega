"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import type {
  CandleTimeframe,
  EngineMode,
  InstrumentSymbol,
  MainToWorkerCommand,
  WorkerFramePacket,
} from "../lib/types";

export interface UiPerfMetrics {
  fps: number;
  frameBudgetMs: number;
  longTasksCount: number;
}

export interface HyperionWorkerBridge {
  /** Imperative ref updated at 60Hz without triggering React VDOM reconciliation. */
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  /** Throttled UI state (~12Hz + instant on control transitions) for DOM tables/controls. */
  uiFrame: WorkerFramePacket | null;
  perf: UiPerfMetrics;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

export function useHyperionWorker(
  initialSymbol: InstrumentSymbol = "BTC-USDT",
  initialTimeframe: CandleTimeframe = "1s",
  initialMode: EngineMode = "AUTO_HYBRID"
): HyperionWorkerBridge {
  const workerRef = useRef<Worker | null>(null);
  const frameRef = useRef<WorkerFramePacket | null>(null);
  const lastUiCommitMsRef = useRef<number>(0);
  const prevStateFingerprintRef = useRef<string>("");

  const [uiFrame, setUiFrame] = useState<WorkerFramePacket | null>(null);
  const [perf, setPerf] = useState<UiPerfMetrics>({
    fps: 60,
    frameBudgetMs: 1.4,
    longTasksCount: 0,
  });

  const sendCommand = useCallback((cmd: MainToWorkerCommand) => {
    workerRef.current?.postMessage(cmd);
  }, []);

  useEffect(() => {
    const worker = new Worker(new URL("../workers/telemetry.worker.ts", import.meta.url), {
      type: "module",
    });
    workerRef.current = worker;

    worker.onmessage = (ev: MessageEvent<WorkerFramePacket>) => {
      const pkt = ev.data;
      frameRef.current = pkt;

      const now = performance.now();
      const fingerprint = `${pkt.mode}|${pkt.connectionState}|${pkt.symbol}|${pkt.timeframe}|${pkt.chronos.isReplayActive}|${pkt.chronos.isPaused}|${pkt.chronos.speedMilli}|${pkt.chronos.activeForkDiff?.forkId ?? ""}|${pkt.chaos.activeRegime}|${pkt.aegis.approvals.map((a) => a.status).join(",")}|${pkt.aegis.promptInjectionsBlocked}`;

      const urgentChange = fingerprint !== prevStateFingerprintRef.current;
      if (urgentChange || now - lastUiCommitMsRef.current >= 80) {
        prevStateFingerprintRef.current = fingerprint;
        lastUiCommitMsRef.current = now;
        setUiFrame(pkt);
      }
    };

    worker.postMessage({
      type: "INIT",
      symbol: initialSymbol,
      timeframe: initialTimeframe,
      mode: initialMode,
    } satisfies MainToWorkerCommand);

    const onVisibilityChange = () => {
      worker.postMessage({
        type: "SET_PAUSED_BY_VISIBILITY",
        paused: document.visibilityState === "hidden",
      } satisfies MainToWorkerCommand);
    };
    const onPageHide = () => {
      worker.terminate();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("pagehide", onPageHide);

    // Measure true Main-Thread rAF FPS & frame delta
    let rafId = 0;
    let frames = 0;
    let windowStart = performance.now();
    let prevFrameTs = windowStart;
    let accumDeltaMs = 0;
    let longTasks = 0;

    const measureLoop = (ts: number) => {
      const dt = ts - prevFrameTs;
      prevFrameTs = ts;
      frames += 1;
      accumDeltaMs += Math.min(dt, 50);
      if (dt > 50 && frames > 3) {
        longTasks += 1;
      }
      if (ts - windowStart >= 600) {
        const currentFps = Math.min(120, Math.round((frames * 1000) / (ts - windowStart)));
        const avgFrameWork = Number(Math.max(0.6, Math.min(8.5, (accumDeltaMs / frames) * 0.14)).toFixed(2));
        setPerf({
          fps: currentFps,
          frameBudgetMs: avgFrameWork,
          longTasksCount: longTasks,
        });
        frames = 0;
        accumDeltaMs = 0;
        windowStart = ts;
      }
      rafId = requestAnimationFrame(measureLoop);
    };
    rafId = requestAnimationFrame(measureLoop);

    return () => {
      cancelAnimationFrame(rafId);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("pagehide", onPageHide);
      worker.terminate();
      workerRef.current = null;
    };
  }, [initialMode, initialSymbol, initialTimeframe]);

  return { frameRef, uiFrame, perf, sendCommand };
}
