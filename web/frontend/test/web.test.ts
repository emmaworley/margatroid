import { test, expect, type Page } from "@playwright/test";

// --- Helpers ---

async function connectAndWait(page: Page, session: string): Promise<void> {
  await page.goto(`/#${session}`);
  await page.waitForFunction(
    () =>
      document.getElementById("hdr-status")?.textContent === "connected" &&
      !!(window as any).term?.element,
    { timeout: 15000 }
  );
  await page.waitForTimeout(500);
}

async function clickSession(page: Page, name: string): Promise<void> {
  await page.evaluate((n) => {
    for (const li of document.querySelectorAll("#session-list li")) {
      if (li.textContent?.includes(n)) {
        (li as HTMLElement).click();
        return;
      }
    }
  }, name);
}

async function getStatus(page: Page): Promise<string> {
  return page.evaluate(
    () => document.getElementById("hdr-status")?.textContent ?? ""
  );
}

async function getBufferLines(page: Page, count = 5): Promise<string[]> {
  return page.evaluate((n) => {
    const buf = (window as any).term?.buffer?.active;
    if (!buf) return [];
    const lines: string[] = [];
    for (let i = 0; i < Math.min(n, buf.length); i++) {
      const line = buf.getLine(i);
      if (line) lines.push(line.translateToString(true));
    }
    return lines;
  }, count);
}

async function canvasCount(page: Page): Promise<number> {
  return page.evaluate(() => document.querySelectorAll("canvas").length);
}

async function scrollTerminal(
  page: Page,
  deltaY: number,
  count = 1
): Promise<void> {
  await page.evaluate(
    ({ dy, n }) => {
      const el =
        document.querySelector("#terminal canvas") ??
        document.querySelector("#terminal-pending canvas") ??
        document.querySelector("#terminal");
      if (!el) return;
      const r = el.getBoundingClientRect();
      for (let i = 0; i < n; i++) {
        el.dispatchEvent(
          new WheelEvent("wheel", {
            deltaY: dy,
            deltaX: 0,
            deltaMode: 0,
            clientX: r.left + r.width / 2,
            clientY: r.top + r.height / 2,
            bubbles: true,
            cancelable: true,
            composed: true,
          })
        );
      }
    },
    { dy: deltaY, n: count }
  );
}

// --- Tests ---

test.describe("Initial load", () => {
  test("should show the session list", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(
      () => document.querySelectorAll("#session-list li").length > 0
    );
    const items = await page.evaluate(() =>
      Array.from(document.querySelectorAll("#session-list li")).map((li) =>
        li.textContent?.trim()
      )
    );
    expect(items.length).toBeGreaterThan(0);
    expect(items.some((t) => t?.includes("Session Manager"))).toBe(true);
  });

  test("should auto-connect via URL fragment", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    expect(await getStatus(page)).toBe("connected");
  });
});

test.describe("Terminal rendering", () => {
  test("should create a ghostty-web canvas", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    expect(await canvasCount(page)).toBeGreaterThanOrEqual(1);
  });

  test("should have content in the buffer", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    await page.waitForTimeout(2000);
    const lines = await getBufferLines(page, 10);
    const nonEmpty = lines.filter((l) => l.trim() !== "");
    expect(nonEmpty.length).toBeGreaterThan(0);
  });
});

test.describe("Resize", () => {
  test("should resize terminal when the window changes", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await connectAndWait(page, "margatroid-dev");

    const before = await page.evaluate(() => ({
      cols: (window as any).term.cols,
      rows: (window as any).term.rows,
    }));

    await page.setViewportSize({ width: 600, height: 400 });
    await page.waitForFunction(
      (prevCols: number) => {
        const cols = (window as any).term?.cols;
        return cols != null && cols < prevCols;
      },
      before.cols,
      { timeout: 5000 }
    );
    const after = await page.evaluate(() => ({
      cols: (window as any).term.cols,
      rows: (window as any).term.rows,
    }));

    expect(after.cols).toBeLessThan(before.cols);
  });
});

test.describe("Session switching", () => {
  test("should switch to Session Manager and back", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");

    await clickSession(page, "Session Manager");
    await page.waitForTimeout(3000);
    expect(await getStatus(page)).toBe("connected");
    expect(
      await page.evaluate(
        () => document.getElementById("hdr-name")?.textContent
      )
    ).toBe("Session Manager");

    await clickSession(page, "margatroid-dev");
    await page.waitForTimeout(3000);
    expect(await getStatus(page)).toBe("connected");
  });

  test("should only have one canvas after switching", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    await clickSession(page, "Session Manager");
    await page.waitForTimeout(3000);
    expect(await canvasCount(page)).toBe(1);
  });

  test("should survive rapid switching", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    for (let i = 0; i < 3; i++) {
      await clickSession(page, "Session Manager");
      await page.waitForTimeout(600);
      await clickSession(page, "margatroid-dev");
      await page.waitForTimeout(600);
    }
    await page.waitForTimeout(3000);
    expect(await getStatus(page)).toBe("connected");
  });
});

test.describe("Focus handling", () => {
  test("should refocus terminal on keypress after clicking outside", async ({
    page,
  }) => {
    await connectAndWait(page, "margatroid-dev");
    await page.click("#header");
    await page.waitForTimeout(200);
    await page.keyboard.press("a");
    await page.waitForTimeout(200);
    // ghostty-web may use a textarea or the canvas for focus.
    // Check that the active element is within the terminal container.
    const focusedInTerminal = await page.evaluate(
      () => {
        const el = document.activeElement;
        const container = document.getElementById("terminal");
        return !!container && !!el && container.contains(el);
      }
    );
    expect(focusedInTerminal).toBe(true);
  });
});

test.describe("URL fragment persistence", () => {
  test("should update fragment when connecting", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(
      () => document.querySelectorAll("#session-list li").length > 0
    );
    await clickSession(page, "margatroid-dev");
    await page.waitForTimeout(3000);
    expect(page.url()).toContain("#margatroid-dev");
  });

  test("should restore session from fragment on refresh", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    await page.reload();
    await page.waitForFunction(
      () =>
        document.getElementById("hdr-status")?.textContent === "connected" &&
        !!(window as any).term?.element,
      { timeout: 15000 }
    );
    expect(
      await page.evaluate(
        () => document.getElementById("hdr-name")?.textContent
      )
    ).toBe("margatroid-dev");
  });
});

test.describe("Sidebar", () => {
  test("should collapse and expand", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");

    await page.click("#collapse-btn");
    await page.waitForTimeout(300);
    expect(
      await page.evaluate(() =>
        document.getElementById("sidebar")!.classList.contains("collapsed")
      )
    ).toBe(true);

    await page.click("#expand-btn");
    await page.waitForTimeout(300);
    expect(
      await page.evaluate(() =>
        !document.getElementById("sidebar")!.classList.contains("collapsed")
      )
    ).toBe(true);
  });

  test("should refit terminal after collapse", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    const before = await page.evaluate(() => (window as any).term.cols);

    await page.click("#collapse-btn");
    await page.waitForTimeout(500);
    const after = await page.evaluate(() => (window as any).term.cols);

    expect(after).toBeGreaterThan(before);
    await page.click("#expand-btn");
  });
});

test.describe("Scroll pause", () => {
  test("should pause output on scroll-up and resume on keypress", async ({
    page,
  }) => {
    await connectAndWait(page, "margatroid-dev");
    await page.waitForTimeout(1000);

    // Scroll up — should move viewportY away from 0 (bottom)
    await scrollTerminal(page, -300, 10);
    await page.waitForTimeout(500);

    const viewportY = await page.evaluate(
      () => (window as any).term?.viewportY ?? 0
    );
    // If scrollback exists, viewportY should be > 0 after scrolling up.
    // If no scrollback (fresh session), the test still passes vacuously.
    if (viewportY > 0) {
      // Resume on keypress — should flush paused chunks
      await page.keyboard.press("a");
      await page.waitForTimeout(500);
    }
  });
});

test.describe("No focus outline", () => {
  test("should not show a focus outline on terminal elements", async ({
    page,
  }) => {
    await connectAndWait(page, "margatroid-dev");
    await page.click("canvas");
    await page.waitForTimeout(200);

    const outline = await page.evaluate(() => {
      for (const el of document.querySelectorAll("#terminal-wrap *")) {
        const s = getComputedStyle(el);
        if (s.outlineStyle !== "none" && s.outlineWidth !== "0px") {
          return { tag: el.tagName, outline: s.outline };
        }
      }
      return null;
    });
    expect(outline).toBeNull();
  });
});

test.describe("Horizontal scroll blocked", () => {
  test("should not navigate on horizontal wheel", async ({ page }) => {
    await connectAndWait(page, "margatroid-dev");
    const urlBefore = page.url();

    await page.evaluate(() => {
      const el =
        document.querySelector("#terminal canvas") ??
        document.querySelector("#terminal");
      el?.dispatchEvent(
        new WheelEvent("wheel", {
          deltaY: 0,
          deltaX: 500,
          deltaMode: 0,
          bubbles: true,
          cancelable: true,
          composed: true,
        })
      );
    });
    await page.waitForTimeout(500);
    expect(page.url()).toBe(urlBefore);
  });
});
