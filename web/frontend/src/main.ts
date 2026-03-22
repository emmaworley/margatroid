import { init, Terminal, FitAddon, type ITerminalOptions, type IDisposable } from "ghostty-web";

await init();

// Frontend version: fetched from the server on first load. On reconnect,
// compared against the server's current version to detect updates.
let loadedVersion: string | null = null;
(async () => {
  try {
    const res = await fetch("/api/version");
    if (res.ok) {
      const data = await res.json();
      loadedVersion = data.version ?? null;
    }
  } catch { /* server may not be up yet */ }
})();
// Expose for tests.
declare global {
  interface Window {
    __BUILD_VERSION__: string | null;
    term: Terminal | null;
    ws: WebSocket | null;
  }
}
Object.defineProperty(window, "__BUILD_VERSION__", { get: () => loadedVersion });

interface SessionInfo {
  name: string;
  image: string;
  status: string;
  connectable: boolean;
}

const TERM_OPTIONS: ITerminalOptions = {
  scrollback: 10000,
  fontSize: 14,
  fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, monospace',
  theme: {
    background: "#0f0f14",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    selectionBackground: "#33467c",
    black: "#15161e", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68",
    blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6",
    brightBlack: "#414868", brightRed: "#f7768e", brightGreen: "#9ece6a",
    brightYellow: "#e0af68", brightBlue: "#7aa2f7", brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff", brightWhite: "#c0caf5",
  },
};

let term: Terminal | null = null;
let fitAddon: FitAddon | null = null;
let ws: WebSocket | null = null;
let currentSession: string | null = null;
let dataDisposable: IDisposable | null = null;
let resizeDisposable: IDisposable | null = null;
let revealTimer: ReturnType<typeof setTimeout> | null = null;
let connectGeneration = 0;

// Expose for tests.
declare global {
  interface Window { term: Terminal | null; }
}

// --- Helpers ---

function $(id: string): HTMLElement {
  return document.getElementById(id)!;
}

function esc(s: string): string {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

function setStatus(cls: string, text: string): void {
  const el = $("hdr-status");
  el.className = cls;
  el.textContent = text;
}

// --- Scroll pause/resume ---

let outputPaused = false;
let pausedChunks: Uint8Array[] = [];

function pauseOutput(): void {
  if (outputPaused) return;
  outputPaused = true;
}

function resumeOutput(): void {
  if (!outputPaused) return;
  outputPaused = false;
  if (term) {
    for (const chunk of pausedChunks) {
      term.write(chunk);
    }
  }
  pausedChunks = [];
}

// Accumulate sub-line scroll deltas so trackpad gestures feel proportional.
// Without this, small deltas (5-10px) all round to 1 line.
let scrollAccum = 0;
const PX_PER_LINE = 20;

document.addEventListener("wheel", (e: WheelEvent) => {
  const wrap = $("terminal-wrap");
  if (!wrap.contains(e.target as Node)) return;

  e.preventDefault();
  e.stopImmediatePropagation();

  if (!term?.element) return;
  if (e.deltaY < 0) pauseOutput();

  scrollAccum += e.deltaY;
  const lines = Math.trunc(scrollAccum / PX_PER_LINE);
  if (lines !== 0) {
    scrollAccum -= lines * PX_PER_LINE;
    term.scrollLines(lines);
  }
}, { passive: false, capture: true });

document.addEventListener("keydown", () => {
  if (outputPaused) resumeOutput();
  if (term?.element && document.activeElement !== (term as any).textarea) {
    term.focus();
  }
});

// --- Sidebar toggle ---

$("collapse-btn").onclick = () => {
  $("sidebar").classList.add("collapsed");
  setTimeout(() => { if (term?.element && fitAddon) fitAddon.fit(); }, 200);
};
$("expand-btn").onclick = () => {
  $("sidebar").classList.remove("collapsed");
  setTimeout(() => { if (term?.element && fitAddon) fitAddon.fit(); }, 200);
};

// --- Session list ---

async function refreshSessions(): Promise<void> {
  try {
    const res = await fetch("/api/sessions");
    const sessions: SessionInfo[] = await res.json();
    renderSessions(sessions);
  } catch (e) {
    console.error("Failed to fetch sessions:", e);
  }
}

function renderSessions(sessions: SessionInfo[]): void {
  const list = $("session-list");
  list.innerHTML = "";

  const manager = sessions.find((s) => s.name === "_manager");
  const others = sessions.filter((s) => s.name !== "_manager");

  if (manager) {
    const li = document.createElement("li");
    if (currentSession === "_manager") li.classList.add("active");
    li.innerHTML = `
      <span class="status-dot running"></span>
      <span class="session-name">Session Manager</span>
    `;
    li.onclick = () => connect("_manager");
    list.appendChild(li);
  }

  if (others.length > 0) {
    const label = document.createElement("div");
    label.className = "section-label";
    label.textContent = "Sessions";
    list.appendChild(label);

    for (const s of others) {
      const li = document.createElement("li");
      if (!s.connectable) li.classList.add("disabled");
      if (s.name === currentSession) li.classList.add("active");

      let badge = "";
      if (s.status !== "running")
        badge = '<span class="session-badge">stopped</span>';

      li.innerHTML = `
        <span class="status-dot ${s.status}"></span>
        <span class="session-name">${esc(s.name)}</span>
        ${badge}
        <span class="session-image">${esc(s.image)}</span>
      `;

      if (s.connectable) {
        li.onclick = () => connect(s.name);
      }
      list.appendChild(li);
    }
  }
}

// --- Terminal connection ---

function connect(sessionId: string): void {
  if (revealTimer) { clearTimeout(revealTimer); revealTimer = null; }
  outputPaused = false;
  pausedChunks = [];
  const thisGeneration = ++connectGeneration;

  cancelReconnect();
  if (ws) {
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
    ws.close();
    ws = null;
  }
  if (dataDisposable) { dataDisposable.dispose(); dataDisposable = null; }
  if (resizeDisposable) { resizeDisposable.dispose(); resizeDisposable = null; }
  currentSession = sessionId;

  history.replaceState(null, "", "#" + encodeURIComponent(sessionId));

  const label = sessionId === "_manager" ? "Session Manager" : sessionId;
  $("placeholder").style.display = "none";
  $("terminal-wrap").style.display = "";
  $("header").style.display = "";
  $("hdr-name").textContent = label;

  // Keep the old terminal visible. Create the new one in a hidden sibling.
  // Once data arrives and the resize repaint settles, swap them atomically.
  const oldContainer = $("terminal");
  const termWrap = $("terminal-wrap");

  const fresh = document.createElement("div");
  fresh.id = "terminal-pending";
  fresh.style.cssText = "position:absolute;top:0;left:0;right:0;bottom:0;visibility:hidden";
  termWrap.style.position = "relative";
  termWrap.appendChild(fresh);

  if (term) { term.dispose(); term = null; fitAddon = null; }
  term = new Terminal(TERM_OPTIONS);
  fitAddon = new FitAddon();
  term.loadAddon(fitAddon);
  term.open(fresh);
  window.term = term;
  fitAddon.fit();

  // Clear WASM renderer state.
  term.write("\x1b[2J\x1b[3J\x1b[H");

  let pendingData: Uint8Array[] | null = [];

  function reveal(): void {
    if (thisGeneration !== connectGeneration) return;
    oldContainer.remove();
    fresh.id = "terminal";
    fresh.style.cssText = "height:100%";
    termWrap.style.position = "";
    if (pendingData) {
      for (const chunk of pendingData) {
        term!.write(chunk);
      }
    }
    pendingData = null;
    fitAddon!.observeResize();
    term!.focus();
  }

  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  ws = new WebSocket(
    `${proto}//${location.host}/ws/${encodeURIComponent(sessionId)}?cols=${term.cols || 80}&rows=${term.rows || 24}`
  );
  ws.binaryType = "arraybuffer";
  window.ws = ws;

  setStatus("connected", "connecting...");

  ws.onopen = () => {
    if (thisGeneration !== connectGeneration) return;
    reconnectAttempt = 0;
    setStatus("connected", "connected");
    const cols = term!.cols || 80;
    const rows = term!.rows || 24;
    ws!.send(JSON.stringify({ type: "resize", cols, rows }));
    revealTimer = setTimeout(reveal, 350);
  };

  ws.onmessage = (e: MessageEvent) => {
    if (thisGeneration !== connectGeneration) return;
    if (e.data instanceof ArrayBuffer) {
      if (pendingData) {
        pendingData.push(new Uint8Array(e.data));
        return;
      }
      const chunk = new Uint8Array(e.data);
      if (outputPaused) {
        pausedChunks.push(chunk);
      } else {
        term!.write(chunk);
      }
    }
  };


  dataDisposable = term.onData((data: string) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(new TextEncoder().encode(data));
    }
  });

  resizeDisposable = term.onResize(({ cols, rows }: { cols: number; rows: number }) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "resize", cols, rows }));
    }
  });

  ws.onclose = () => {
    ws = null;
    // Only auto-reconnect if this is still the active connection
    // (not superseded by a user-initiated session switch).
    if (thisGeneration === connectGeneration && currentSession === sessionId) {
      scheduleReconnect(sessionId);
    } else {
      setStatus("disconnected", "disconnected");
    }
  };
  ws.onerror = () => {
    // onclose fires after onerror, so reconnect is handled there.
  };

  refreshSessions();
}

// --- Auto-reconnect ---

let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;

function scheduleReconnect(sessionId: string): void {
  reconnectAttempt++;
  const delay = Math.min(1000 * Math.pow(1.5, reconnectAttempt - 1), 10000);
  setStatus("disconnected", `reconnecting... (${reconnectAttempt})`);

  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    if (currentSession !== sessionId) return; // Switched away
    connect(sessionId);
    checkVersion();
  }, delay);
}

function cancelReconnect(): void {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  reconnectAttempt = 0;
}

async function checkVersion(): Promise<void> {
  try {
    const res = await fetch("/api/version");
    if (!res.ok) return;
    const data = await res.json();
    if (data.version && loadedVersion && data.version !== loadedVersion) {
      showUpdateBanner();
    }
  } catch {
    // Server unreachable — will retry on next reconnect.
  }
}

function showUpdateBanner(): void {
  if (document.getElementById("update-banner")) return;
  const banner = document.createElement("span");
  banner.id = "update-banner";
  banner.innerHTML = `Update available · <a href="#" id="update-reload">reload</a>`;
  document.getElementById("header")?.appendChild(banner);
  document.getElementById("update-reload")?.addEventListener("click", (e) => {
    e.preventDefault();
    location.reload();
  });
}

// --- Init ---

await refreshSessions();

const fragment = decodeURIComponent(location.hash.slice(1));
if (fragment) {
  connect(fragment);
}

setInterval(refreshSessions, 5000);

document.addEventListener("mousedown", (e: MouseEvent) => {
  if (!term?.element) return;
  if ((e.target as HTMLElement).closest("button, li")) return;
  term.focus();
});
