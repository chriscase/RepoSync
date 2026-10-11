import { defineConfig } from "@playwright/test";

const suite = process.env.LAB_SUITES || "smoke";
const selected = new Set(suite.split(",").map((item) => item.trim()).filter(Boolean));
const pattern = selected.has("full")
  ? /@smoke|@full/
  : selected.has("soak")
    ? /@soak/
    : /@smoke/;

export default defineConfig({
  testDir: "./specs",
  timeout: 300_000,
  expect: { timeout: 30_000 },
  fullyParallel: false,
  workers: Number(process.env.LAB_WORKERS || "1"),
  retries: 0,
  forbidOnly: true,
  grep: pattern,
  reporter: [
    ["list"],
    ["json", { outputFile: process.env.LAB_PLAYWRIGHT_JSON || "playwright-report.json" }],
  ],
  outputDir: process.env.LAB_PLAYWRIGHT_OUTPUT || "test-results",
  use: {
    baseURL: process.env.LAB_UI_URL || "http://127.0.0.1:9",
    ignoreHTTPSErrors: false,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
  },
});
