import { test } from "@playwright/test";
import catalog from "../catalog/operations.json" with { type: "json" };
import { runCase } from "../playwright/handlers.ts";
import { Lab } from "../playwright/runtime.ts";

const requested = new Set((process.env.LAB_SUITES || "smoke").split(",").map((item) => item.trim()).filter(Boolean));
const selected = new Set(requested);
if (selected.has("full")) selected.add("smoke");
const operations = catalog.operations.filter((operation) => operation.disposition === "execute" && selected.has(operation.suite));

test.describe.configure({ mode: "serial" });

for (const operation of operations) {
  test(operation.id, { tag: `@${operation.suite}` }, async ({ page }, info) => {
    info.annotations.push({ type: "labCase", description: operation.id });
    if (operation.suite === "soak" || operation.id === "GIT-OVER-1000") info.setTimeout(1_200_000);
    const lab = new Lab(page, info);
    page.on("console", (message) => {
      if (message.type() === "error") info.annotations.push({ type: "console", description: message.text().slice(0, 300) });
    });
    page.on("response", (response) => {
      if (response.status() >= 500) info.annotations.push({ type: "http", description: `${response.status()} ${response.url()}` });
    });
    await runCase(lab, operation);
  });
}
