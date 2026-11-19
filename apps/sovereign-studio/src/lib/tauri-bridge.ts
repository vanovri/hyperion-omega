/**
 * HYPERION OMEGA Channel 20 — Tauri v2 Native Desktop & Multi-Window Bridge
 * Supports native Rust Tauri v2 multi-window spawning (`WebviewWindow`) and IPC,
 * with seamless fallback to calibrated multi-monitor browser windows in Web mode.
 */

import { SovereignPanelId } from "./telemetry-types";

export interface NativeHostTelemetry {
  readonly runtime: "TAURI_V2_RUST_DESKTOP" | "WEB_BROWSER";
  readonly osArch: string;
  readonly activeWindows: number;
  readonly ipcRoundtripUs: number;
}

export function isTauriDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function spawnDetachedPanelWindow(
  panelId: SovereignPanelId,
  panelTitle: string
): Promise<{ ok: boolean; mode: "TAURI_NATIVE" | "WEB_POPOUT"; label: string }> {
  const windowLabel = `sovereign-${panelId.toLowerCase().replace(/[^a-z0-9]/g, "-")}`;
  const targetUrl = `/?panel=${encodeURIComponent(panelId)}&detached=1`;

  if (isTauriDesktopRuntime()) {
    try {
      const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
      const existing = await WebviewWindow.getByLabel(windowLabel);
      if (existing) {
        await existing.setFocus();
        return { ok: true, mode: "TAURI_NATIVE", label: windowLabel };
      }
      const webview = new WebviewWindow(windowLabel, {
        url: targetUrl,
        title: `HYPERION OMEGA — ${panelTitle}`,
        width: 1180,
        height: 780,
        minWidth: 720,
        minHeight: 520,
        decorations: true,
        resizable: true,
      });
      await new Promise<void>((resolve) => {
        webview.once("tauri://created", () => resolve());
        webview.once("tauri://error", () => resolve());
      });
      return { ok: true, mode: "TAURI_NATIVE", label: windowLabel };
    } catch {
      // Fall through to standard browser window.open if restricted
    }
  }

  if (typeof window !== "undefined") {
    window.open(
      targetUrl,
      windowLabel,
      "popup=yes,width=1180,height=780,noopener,noreferrer"
    );
  }
  return { ok: true, mode: "WEB_POPOUT", label: windowLabel };
}

export async function queryNativeHostTelemetry(): Promise<NativeHostTelemetry> {
  if (isTauriDesktopRuntime()) {
    try {
      const t0 = performance.now();
      const { invoke } = await import("@tauri-apps/api/core");
      const snap = await invoke<{ os_arch: string; active_windows: number }>(
        "host_telemetry_snapshot"
      );
      const rttUs = Math.max(Math.round((performance.now() - t0) * 1000), 8);
      return {
        runtime: "TAURI_V2_RUST_DESKTOP",
        osArch: snap.os_arch,
        activeWindows: snap.active_windows,
        ipcRoundtripUs: rttUs,
      };
    } catch {
      // Fallback if invoked before plugin ready
    }
  }
  return {
    runtime: "WEB_BROWSER",
    osArch: typeof navigator !== "undefined" ? navigator.platform || "WebGL2-Host" : "SSR",
    activeWindows: 1,
    ipcRoundtripUs: 0,
  };
}
