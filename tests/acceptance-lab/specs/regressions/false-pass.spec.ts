import { test, expect } from "@playwright/test";

test("annotated pass then fails", async ({}, info) => {
  info.annotations.push({ type: "labCase", description: "LAB_FALSE_PASS" });
  info.annotations.push({ type: "labStatus", description: "PASS" });
  info.annotations.push({ type: "labReason", description: "annotated pass" });
  expect(false, "disposable fixture: failure after a PASS annotation").toBe(true);
});
