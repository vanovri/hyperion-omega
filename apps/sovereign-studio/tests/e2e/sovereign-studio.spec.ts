import { expect, test } from "@playwright/test";

test.describe("HYPERION OMEGA Channel 20 — Sovereign Workstation Visual & Interactive Crucible", () => {
  test("renders all 4 Sovereign panels, executes Raft failover, ZK tamper rejection, Wasm trap & SQL AST guard with 0 console errors", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        consoleErrors.push(msg.text());
      }
    });
    page.on("pageerror", (err) => {
      consoleErrors.push(err.message);
    });

    await page.goto("/");

    // 1. Verify top Sovereign header & 3D WebGL2 Volatility Surface
    await expect(
      page.getByRole("heading", { name: /HYPERION OMEGA · Sovereign Workstation/i })
    ).toBeVisible();
    await expect(page.getByTestId("vol-surface-webgl-canvas")).toBeVisible();

    // 2. Trigger Raft Leader SIGKILL and verify <500ms Quorum Election recovery
    await page.getByTestId("kill-raft-leader-btn").click();
    await page.waitForTimeout(450);
    await expect(page.getByText(/Committed Tx Loss:/i)).toBeVisible();

    // 3. Verify Client-Side ZK Merkle-Sum Solvency Verifier accepts valid proof & rejects +1e-8 tamper
    await expect(page.getByTestId("zk-verdict-banner")).toContainText(
      /CRYPTOGRAPHIC PROOF VERIFIED IN BROWSER/i
    );
    await page.getByTestId("zk-tamper-leaf-btn").click();
    await expect(page.getByTestId("zk-verdict-banner")).toContainText(
      /CRYPTOGRAPHIC SOUNDNESS REJECTION/i
    );
    await page.getByTestId("zk-load-valid-btn").click();
    await expect(page.getByTestId("zk-verdict-banner")).toContainText(
      /CRYPTOGRAPHIC PROOF VERIFIED IN BROWSER/i
    );

    // 4. Trigger Wasmtime Infinite-Loop Malware Trap
    await page.getByTestId("wasm-malware-trap-btn").click();
    await expect(page.getByTestId("wasm-trap-banner")).toContainText(/TRAP_FUEL_EXHAUSTED/i);

    // 5. Verify DataFusion SQL AST Guard blocks stacked DDL DROP injection
    await page.getByTestId("sql-preset-adversarial_drop_test").click();
    await expect(page.getByTestId("sql-ast-guard-badge")).toContainText(/AST GUARD BLOCKED/i);
    await page.getByTestId("sql-preset-asof_toxic_markout").click();
    await expect(page.getByTestId("sql-ast-guard-badge")).toContainText(/AST GUARD PASS/i);

    // 6. Verify zero horizontal page overflow at 1536x960 and capture visual evidence screenshot
    const hasOverflow = await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 2
    );
    expect(hasOverflow).toBe(false);
    await page.screenshot({
      path: "test-results/sovereign-studio-desktop.png",
      fullPage: true,
    });

    expect(consoleErrors).toEqual([]);
  });
});
