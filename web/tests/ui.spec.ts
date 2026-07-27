import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "HIDShiftへ接続" })).toBeVisible();
});

test("daily navigation remains understandable while disconnected", async ({ page }) => {
  await expect(page.getByRole("heading", { name: "ホーム", exact: true })).toBeVisible();

  await page.locator(".nav-button:visible").filter({ hasText: "接続先" }).click();
  await expect(page.getByRole("heading", { name: "接続先", exact: true })).toBeVisible();

  await page.locator(".nav-button:visible").filter({ hasText: /入力/ }).click();
  await expect(page.getByRole("heading", { name: "入力機器", exact: true })).toBeVisible();

  await page.locator(".nav-button:visible").filter({ hasText: "設定" }).click();
  await expect(page.getByRole("heading", { name: "設定", exact: true })).toBeVisible();
});

test("language and theme preferences apply immediately", async ({ page }) => {
  await page.locator(".nav-button:visible").filter({ hasText: "設定" }).click();
  await page.getByRole("combobox").first().selectOption("en");
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();

  await page.locator(".setting-control select").nth(1).selectOption("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.reload();
  await expect(page.getByRole("heading", { name: "Home", exact: true })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("home has stable responsive screenshots", async ({ page }, testInfo) => {
  await expect(page.locator(".app-shell")).toBeVisible();
  expect(await page.screenshot({ fullPage: true, animations: "disabled" })).toMatchSnapshot(
    `home-${testInfo.project.name}.png`,
  );
});
