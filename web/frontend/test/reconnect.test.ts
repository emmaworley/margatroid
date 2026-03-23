/**
 * Tests for WebSocket auto-reconnect and version-change detection.
 * Uses margatroid-dev (always running) to avoid test relay setup issues.
 */
import { test, expect, type Page } from "@playwright/test";

async function connectAndWait(page: Page): Promise<void> {
  await page.goto("/#margatroid-dev");
  await page.waitForFunction(
    () =>
      document.getElementById("hdr-status")?.textContent === "connected" &&
      !!(window as any).term?.element,
    { timeout: 15000 }
  );
  await page.waitForTimeout(500);
}

async function getStatus(page: Page): Promise<string> {
  return page.evaluate(
    () => document.getElementById("hdr-status")?.textContent ?? ""
  );
}

test.describe("Auto-reconnect", () => {
  test("should show reconnecting status when WebSocket closes", async ({ page }) => {
    await connectAndWait(page);
    expect(await getStatus(page)).toBe("connected");

    // Capture status changes via a MutationObserver before closing
    await page.evaluate(() => {
      (window as any).__statusHistory__ = [];
      const el = document.getElementById("hdr-status")!;
      new MutationObserver(() => {
        (window as any).__statusHistory__.push(el.textContent);
      }).observe(el, { childList: true, characterData: true, subtree: true });
    });

    await page.evaluate(() => {
      (window as any).ws?.close();
    });

    // Wait for reconnect to complete
    await page.waitForFunction(
      () => document.getElementById("hdr-status")?.textContent === "connected",
      { timeout: 10000 }
    );

    const history = await page.evaluate(() => (window as any).__statusHistory__);
    // Should have gone through "reconnecting" at some point
    expect(history.some((s: string) => s.includes("reconnecting"))).toBe(true);
  });

  test("should automatically reconnect after WebSocket close", async ({ page }) => {
    await connectAndWait(page);

    await page.evaluate(() => {
      (window as any).ws?.close();
    });

    // Wait for reconnect — status must reach "connected" at some point.
    const reconnected = await page.waitForFunction(
      () => document.getElementById("hdr-status")?.textContent === "connected",
      { timeout: 10000 }
    ).then(() => true).catch(() => false);

    expect(reconnected).toBe(true);
  });

  test("should not reconnect after user-initiated session switch", async ({ page }) => {
    await connectAndWait(page);

    // Switch to manager (intentional close)
    await page.evaluate(() => {
      for (const li of document.querySelectorAll("#session-list li")) {
        if (li.textContent?.includes("Session Manager")) {
          (li as HTMLElement).click();
          return;
        }
      }
    });
    await page.waitForTimeout(3000);

    const name = await page.evaluate(
      () => document.getElementById("hdr-name")?.textContent
    );
    expect(name).toBe("Session Manager");
    expect(await getStatus(page)).toBe("connected");
  });
});

test.describe("Version change detection", () => {
  test("should have a loaded version from the server", async ({ page }) => {
    await connectAndWait(page);
    // Wait for the async version fetch to complete
    await page.waitForTimeout(1000);

    const version = await page.evaluate(() => (window as any).__BUILD_VERSION__);
    expect(version).toBeTruthy();
    expect(typeof version).toBe("string");
    expect(version.length).toBeGreaterThan(0);
  });

  test("should expose version at /api/version", async ({ page }) => {
    const response = await page.request.get("/api/version");
    expect(response.ok()).toBe(true);
    const data = await response.json();
    expect(data.version).toBeTruthy();
    expect(data.version.length).toBeGreaterThan(0);
  });

  test("should not show update banner when versions match", async ({ page }) => {
    await connectAndWait(page);

    // Force a reconnect
    await page.evaluate(() => {
      (window as any).ws?.close();
    });
    await page.waitForFunction(
      () => document.getElementById("hdr-status")?.textContent === "connected",
      { timeout: 10000 }
    );

    // No update banner should be visible
    const banner = await page.evaluate(
      () => document.getElementById("update-banner")
    );
    expect(banner).toBeNull();
  });
});
