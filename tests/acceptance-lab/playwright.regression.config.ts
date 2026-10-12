import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./specs/regressions",
  timeout: 30_000,
  retries: 0,
  reporter: [["json", { outputFile: process.env.LAB_REGRESSION_JSON || "regression-playwright.json" }]],
  projects: [{ name: "node" }],
});
