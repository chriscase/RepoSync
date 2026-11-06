import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./specs/regressions",
  timeout: 30_000,
  retries: 0,
  use: {launchOptions: {executablePath: process.env.LAB_REGRESSION_BROWSER || undefined}},
  reporter: [["json", { outputFile: process.env.LAB_REGRESSION_JSON || "regression-playwright.json" }]],
  outputDir: process.env.LAB_REGRESSION_OUTPUT || "test-results",
  projects: [{ name: "node" }],
});
