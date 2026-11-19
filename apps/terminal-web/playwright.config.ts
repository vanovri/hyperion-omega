import fs from "node:fs";
import path from "node:path";
import { defineConfig, devices } from "@playwright/test";

function resolveLocalChromiumExecutable(): string | undefined {
  if (process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH) {
    return process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH;
  }
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) {
    const msPwDir = path.join(localAppData, "ms-playwright");
    if (fs.existsSync(msPwDir)) {
      const entries = fs
        .readdirSync(msPwDir)
        .filter((d) => d.startsWith("chromium-"))
        .sort()
        .reverse();
      for (const dir of entries) {
        for (const sub of ["chrome-win64", "chrome-win"]) {
          const candidate = path.join(msPwDir, dir, sub, "chrome.exe");
          if (fs.existsSync(candidate)) {
            return candidate;
          }
        }
      }
    }
  }
  return undefined;
}

const executablePath = resolveLocalChromiumExecutable();

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  timeout: 60_000,
  expect: {
    timeout: 10_000,
  },
  reporter: [["list"]],
  use: {
    baseURL: process.env.PLAYWRIGHT_BASE_URL ?? "http://127.0.0.1:3000",
    trace: "off",
    screenshot: "only-on-failure",
    viewport: { width: 1920, height: 1080 },
    colorScheme: "dark",
    launchOptions: executablePath ? { executablePath } : undefined,
  },
  projects: [
    {
      name: "chromium-desktop",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1920, height: 1080 } },
    },
  ],
  webServer: process.env.PLAYWRIGHT_SKIP_WEBSERVER
    ? undefined
    : {
        command: "node ./node_modules/next/dist/bin/next start -H 127.0.0.1 -p 3000",
        url: "http://127.0.0.1:3000",
        reuseExistingServer: true,
        stdout: "pipe",
        stderr: "pipe",
        timeout: 90_000,
      },
});
