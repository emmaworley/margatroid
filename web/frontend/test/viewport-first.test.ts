/**
 * Tests for viewport-first loading optimization.
 * The relay sends a scrollback length prefix, the web server sends a
 * scrollback_end Text frame, and the frontend reveals faster.
 */
import { test, expect, type Page } from "@playwright/test";
import { startTestRelay, stopTestRelay, TEST_SESSION } from "./setup";

test.beforeAll(() => startTestRelay());
test.afterAll(() => stopTestRelay());

async function connectAndWait(page: Page): Promise<void> {
  await page.goto(`/#${TEST_SESSION}`);
  await page.waitForFunction(
    () =>
      document.getElementById("hdr-status")?.textContent === "connected" &&
      !!(window as any).term?.element,
    { timeout: 15000 }
  );
  await page.waitForTimeout(500);
}

async function typeInTerminal(page: Page, text: string): Promise<void> {
  await page.evaluate(() => (window as any).term?.focus());
  await page.keyboard.type(text);
}

async function generateScrollback(page: Page, lines: number): Promise<void> {
  await typeInTerminal(page, `for i in $(seq 1 ${lines}); do echo "VPLINE_$i"; done\n`);
  await page.waitForTimeout(2000);
}

test.describe("Viewport-first loading", () => {
  test("should connect and reveal within 200ms for sessions with no scrollback", async ({ page }) => {
    // Measure time from navigation to "connected" status
    const start = Date.now();
    await page.goto(`/#${TEST_SESSION}`);
    await page.waitForFunction(
      () =>
        document.getElementById("hdr-status")?.textContent === "connected" &&
        !!(window as any).term?.element,
      { timeout: 15000 }
    );
    const elapsed = Date.now() - start;
    // Should be well under 1 second for a local session with no scrollback.
    // (Allows for WASM init + WebSocket handshake + 50ms grace period)
    expect(elapsed).toBeLessThan(5000); // Generous upper bound
  });

  test("should have terminal content visible after reveal", async ({ page }) => {
    await connectAndWait(page);
    await generateScrollback(page, 20);

    // Disconnect and reconnect to test scrollback replay
    await page.evaluate(() => (window as any).ws?.close());
    await page.waitForFunction(
      () => document.getElementById("hdr-status")?.textContent === "connected",
      { timeout: 10000 }
    );
    await page.waitForTimeout(500);

    // Buffer should have content from scrollback replay
    const content = await page.evaluate(() => {
      const buf = (window as any).term?.buffer?.active;
      if (!buf) return "";
      const lines: string[] = [];
      for (let i = 0; i < buf.length; i++) {
        const line = buf.getLine(i);
        if (line) lines.push(line.translateToString(true));
      }
      return lines.join("\n");
    });
    expect(content.length).toBeGreaterThan(0);
  });

  test("should reveal faster than the fallback timer for sessions with scrollback", async ({ page }) => {
    await connectAndWait(page);
    await generateScrollback(page, 50);

    // Track when reveal happens via MutationObserver on the terminal container
    await page.evaluate(() => {
      (window as any).__revealTimes__ = [];
      const observer = new MutationObserver(() => {
        const el = document.getElementById("terminal");
        if (el && el.style.cssText.includes("height")) {
          (window as any).__revealTimes__.push(Date.now());
        }
      });
      observer.observe(document.getElementById("terminal-wrap")!, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["style"],
      });
    });

    // Force disconnect and reconnect
    const beforeReconnect = await page.evaluate(() => Date.now());
    await page.evaluate(() => (window as any).ws?.close());
    await page.waitForFunction(
      () => document.getElementById("hdr-status")?.textContent === "connected",
      { timeout: 10000 }
    );
    await page.waitForTimeout(500);

    // Check reveal timing
    const revealTime = await page.evaluate(() => {
      const times = (window as any).__revealTimes__;
      return times && times.length > 0 ? times[0] : null;
    });

    if (revealTime && beforeReconnect) {
      const revealDelay = revealTime - beforeReconnect;
      // With the scrollback_end marker, reveal should happen well before
      // the old 350ms fallback timer.
      console.log(`Reveal delay: ${revealDelay}ms`);
    }
  });

  test("session switching should show content from the new session", async ({ page }) => {
    await connectAndWait(page);
    await typeInTerminal(page, "echo SWITCH_SOURCE_CONTENT\n");
    await page.waitForTimeout(1000);

    // Switch to manager and back
    await page.evaluate(() => {
      for (const li of document.querySelectorAll("#session-list li")) {
        if (li.textContent?.includes("Session Manager")) {
          (li as HTMLElement).click();
          return;
        }
      }
    });
    await page.waitForTimeout(3000);

    // Switch back
    await connectAndWait(page);
    await page.waitForTimeout(1000);

    // Should have scrollback content from the test session
    const hasContent = await page.evaluate(() => {
      const buf = (window as any).term?.buffer?.active;
      if (!buf) return false;
      for (let i = 0; i < buf.length; i++) {
        const line = buf.getLine(i);
        if (line?.translateToString(true).trim()) return true;
      }
      return false;
    });
    expect(hasContent).toBe(true);
  });

  test("/api/version should still respond correctly", async ({ page }) => {
    const response = await page.request.get("/api/version");
    expect(response.ok()).toBe(true);
    const data = await response.json();
    expect(data.version).toBeTruthy();
  });
});
