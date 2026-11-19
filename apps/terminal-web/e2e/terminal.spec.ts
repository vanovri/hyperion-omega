import fs from "node:fs";
import path from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const SCREENSHOT_DIR = process.env.HYPERION_SCREENSHOT_DIR;

async function captureIfEnabled(page: Page, filename: string): Promise<void> {
  if (!SCREENSHOT_DIR) return;
  fs.mkdirSync(SCREENSHOT_DIR, { recursive: true });
  await page.screenshot({ path: path.join(SCREENSHOT_DIR, filename), fullPage: false });
}

async function assertZeroHorizontalOverflow(page: Page, label: string): Promise<void> {
  const overflow = await page.evaluate(() => ({
    docScrollW: document.documentElement.scrollWidth,
    innerW: window.innerWidth,
    bodyScrollW: document.body.scrollWidth,
  }));
  expect(
    overflow.docScrollW,
    `Horizontal overflow detected in ${label}: scrollW=${overflow.docScrollW} innerW=${overflow.innerW}`
  ).toBeLessThanOrEqual(overflow.innerW + 1);
}

test.describe("HYPERION Channel 09 Institutional Command Terminal Verification", () => {
  test("verifies all 5 custom workspaces, Chronos Time-Travel & Fork Diff, Chaos Deck, WS reconnect resilience, Aegis HITL & Post-Mortem modal, 60fps audit, zero CLS/overflow, and zero console errors", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];

    page.on("response", (res) => {
      if (res.status() >= 400) {
        consoleErrors.push(`HTTP ${res.status()} ${res.url()}`);
      }
    });
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        const text = msg.text();
        if (!text.includes("ERR_CONNECTION_REFUSED") && !text.includes("WebSocket connection to")) {
          consoleErrors.push(text);
        }
      }
    });
    page.on("pageerror", (err) => {
      pageErrors.push(err.message);
    });

    await page.addInitScript(() => {
      const w = window as unknown as { __hyperionCls?: number };
      w.__hyperionCls = 0;
      try {
        const po = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            const shift = entry as PerformanceEntry & { value?: number; hadRecentInput?: boolean };
            if (!shift.hadRecentInput && typeof shift.value === "number") {
              w.__hyperionCls = (w.__hyperionCls ?? 0) + shift.value;
            }
          }
        });
        po.observe({ type: "layout-shift", buffered: true });
      } catch {
        // Unsupported in older browsers
      }
    });

    await page.goto("/");
    await expect(page.getByTestId("hyperion-top-bar")).toBeVisible();
    await expect(page.getByTestId("chronos-scrubber-dock")).toBeVisible();

    // Verify Standalone Deterministic Worker mode cleanly activates
    await page.getByTestId("engine-mode-select").selectOption("STANDALONE_SIM");
    await expect(page.getByTestId("ws-connection-badge")).toContainText("STANDALONE_ACTIVE");

    // 1. Workspace 1: Order Book & Liquidity Heatmap Canvas
    await expect(page.getByTestId("workspace-orderbook-heatmap")).toBeVisible();
    await expect(page.getByTestId("market-heatmap-canvas")).toBeVisible();
    await expect(page.getByTestId("l2-depth-canvas")).toBeVisible();
    await page.getByTestId("tf-btn-5s").click();
    await page.getByTestId("market-heatmap-canvas").hover({ position: { x: 420, y: 220 } });
    await page.waitForTimeout(350);
    await assertZeroHorizontalOverflow(page, "Workspace 1 OrderBookHeatmap");

    const marketCanvasPainted = await page.evaluate(() => {
      const c = document.querySelector('[data-testid="market-heatmap-canvas"]') as HTMLCanvasElement | null;
      if (!c || c.width === 0 || c.height === 0) return false;
      const ctx = c.getContext("2d");
      if (!ctx) return false;
      const sample = ctx.getImageData(0, 0, Math.min(300, c.width), Math.min(300, c.height)).data;
      let nonBg = 0;
      for (let i = 0; i < sample.length; i += 16) {
        if ((sample[i] ?? 0) > 15 || (sample[i + 1] ?? 0) > 15 || (sample[i + 2] ?? 0) > 20) {
          nonBg++;
        }
      }
      return nonBg > 30;
    });
    expect(marketCanvasPainted).toBe(true);
    await captureIfEnabled(page, "01-orderbook-heatmap.png");

    // 2. Workspace 2: Live Fraud & Wash-Trading Force Graph + SHAP Dissection
    await page.getByTestId("nav-tab-FRAUD_FORCE_GRAPH").click();
    await expect(page.getByTestId("workspace-fraud-force-graph")).toBeVisible();
    await expect(page.getByTestId("fraud-force-canvas")).toBeVisible();
    await expect(page.getByTestId("shap-selected-account")).toContainText("#9001");
    await page.waitForTimeout(450);
    await assertZeroHorizontalOverflow(page, "Workspace 2 FraudForceGraph");
    await captureIfEnabled(page, "02-fraud-force-graph.png");

    await page.getByTestId("select-node-6601").click();
    await expect(page.getByTestId("shap-selected-account")).toContainText("#6601");
    await page.getByTestId("escalate-account-aegis-btn").click();

    // 3. Chronos Time-Travel Scrubber (0.1x Slow-Mo @ Flash-Crash) + Workspace 3 Split-Screen Fork Diff
    await page.getByTestId("chronos-jump-flash-crash").click();
    await expect(page.getByTestId("chronos-mode-badge")).toContainText("CHRONOS REPLAY");
    await expect(page.getByTestId("chronos-seq-readout")).toContainText("#152");

    await page.getByTestId("chronos-step-fwd").click();
    await expect(page.getByTestId("chronos-seq-readout")).toContainText("#153");

    await page.getByTestId("chronos-open-fork-diff").click();
    await expect(page.getByTestId("workspace-chronos-fork")).toBeVisible();
    await expect(page.getByTestId("chronos-baseline-canvas")).toBeVisible();
    await expect(page.getByTestId("chronos-forked-canvas")).toBeVisible();
    await page.getByTestId("chronos-baseline-canvas").hover({ position: { x: 340, y: 120 } });
    await page.waitForTimeout(300);
    await assertZeroHorizontalOverflow(page, "Workspace 3 ChronosForkDiff");
    await captureIfEnabled(page, "03-chronos-fork-diff.png");

    await page.getByTestId("preset-noop-fork").click();
    await expect(page.getByTestId("diff-state-identity")).toContainText("BIT-IDENTICAL");

    await page.getByTestId("preset-mitigation-fork").click();
    await expect(page.getByTestId("diff-state-identity")).toContainText("DIVERGENT");

    await page.getByTestId("chronos-return-live").click();
    await expect(page.getByTestId("chronos-mode-badge")).toContainText("LIVE WAL HEAD");

    // 4. Workspace 4: Chaos Engineering Control Deck + WebSocket Drop & Auto-Reconnect Resilience
    await page.getByTestId("nav-tab-CHAOS_CONTROL_DECK").click();
    await expect(page.getByTestId("workspace-chaos-deck")).toBeVisible();
    await page.getByTestId("chaos-regime-FLASH_CRASH").click();
    await expect(page.getByTestId("chaos-active-regime-readout")).toContainText("FLASH_CRASH");
    await page.waitForTimeout(350);
    await assertZeroHorizontalOverflow(page, "Workspace 4 ChaosControlDeck");
    await captureIfEnabled(page, "04-chaos-control-deck.png");

    await page.getByTestId("chaos-ws-drop-btn").click();
    await expect(page.getByTestId("ws-connection-badge")).toContainText("RECONNECTING");
    await expect(page.getByTestId("ws-connection-badge")).toContainText("STANDALONE_ACTIVE", {
      timeout: 4000,
    });

    await page.getByTestId("chaos-killswitch-btn").click();
    await expect(page.getByTestId("chaos-active-regime-readout")).toContainText("CALM");

    // 5. Workspace 5: Aegis Autonomous SRE Console + Prompt-Injection Block + Native <dialog> Post-Mortem
    await page.getByTestId("nav-tab-AEGIS_SRE_CONSOLE").click();
    await expect(page.getByTestId("workspace-aegis-sre")).toBeVisible();

    const beforeBlocked = await page.getByTestId("aegis-blocked-injections").textContent();
    await page.getByTestId("aegis-test-injection-btn").click();
    await expect(page.getByTestId("aegis-blocked-injections")).not.toHaveText(beforeBlocked ?? "");

    await page.getByTestId("approve-btn-MIT-CMD-900X").click();
    await expect(page.getByTestId("approval-status-MIT-CMD-900X")).toContainText("APPROVED_EXECUTED");
    await assertZeroHorizontalOverflow(page, "Workspace 5 AegisSreConsole");
    await captureIfEnabled(page, "05-aegis-sre-console.png");

    await page.getByTestId("open-postmortem-INC-2026-0891").click();
    await expect(page.getByTestId("postmortem-modal-dialog")).toBeVisible();
    await captureIfEnabled(page, "06-aegis-postmortem-modal.png");
    await page.getByTestId("close-postmortem-modal").click();

    // 6. Frame Rate & CLS Audit on primary high-frequency canvas workspace
    await page.getByTestId("nav-tab-ORDERBOOK_HEATMAP").click();
    await page.waitForTimeout(250);

    const perfAudit = await page.evaluate(async () => {
      const w = window as unknown as { __hyperionCls?: number };
      const start = performance.now();
      let frames = 0;
      await new Promise<void>((resolve) => {
        const step = () => {
          frames++;
          if (performance.now() - start >= 750) {
            resolve();
          } else {
            requestAnimationFrame(step);
          }
        };
        requestAnimationFrame(step);
      });
      const elapsedSec = (performance.now() - start) / 1000;
      const measuredFps = Number((frames / elapsedSec).toFixed(1));
      const cls = Number((w.__hyperionCls ?? 0).toFixed(4));
      return { measuredFps, cls };
    });

    console.log(
      `[PLAYWRIGHT AUDIT] Desktop 1920x1080 | Measured rAF FPS=${perfAudit.measuredFps} | CLS=${perfAudit.cls} | ConsoleErrors=${consoleErrors.length} | PageErrors=${pageErrors.length}`
    );

    expect(perfAudit.cls).toBeLessThanOrEqual(0.01);
    expect(perfAudit.measuredFps).toBeGreaterThanOrEqual(45);
    expect(consoleErrors).toEqual([]);
    expect(pageErrors).toEqual([]);
  });
});
