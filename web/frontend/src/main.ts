import { init, Terminal, FitAddon, type ITerminalOptions, type IDisposable } from "ghostty-web";

await init();

const textEncoder = new TextEncoder();

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
  fontSize: 12,
  fontFamily: 'Monaco, Consolas, "JetBrains Mono", "Fira Code", Menlo, monospace',
  theme: {
    background: "#000000",
    foreground: "#bbbbbb",
    cursor: "#bbbbbb",
    cursorAccent: "#ffffff",
    selectionBackground: "#b4d5ff",
    selectionForeground: "#000000",
    black: "#000000", red: "#bb0000", green: "#00bb00", yellow: "#bbbb00",
    blue: "#0000bb", magenta: "#bb00bb", cyan: "#00bbbb", white: "#bbbbbb",
    brightBlack: "#555555", brightRed: "#ff5555", brightGreen: "#55ff55",
    brightYellow: "#ffff55", brightBlue: "#5555ff", brightMagenta: "#ff55ff",
    brightCyan: "#55ffff", brightWhite: "#ffffff",
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

// Capture-phase keydown: pass browser shortcuts through (Cmd+T, Cmd+1-9,
// etc.) while keeping terminal control keys. Also handle Shift+Enter
// for Claude Code multiline input (sends CSI 13;2u).
document.addEventListener("keydown", (e: KeyboardEvent) => {
  // Shift+Enter → send CSI u encoding for Kitty keyboard protocol.
  // Claude Code uses this to distinguish multiline input from submit.
  if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      e.preventDefault();
      e.stopImmediatePropagation();
      ws.send(textEncoder.encode("\x1b[13;2u"));
      return;
    }
  }
  if (e.metaKey || (e.ctrlKey && !e.altKey)) {
    const key = e.key.toLowerCase();
    const terminalKeys = new Set([
      "c", "v", "a", "z", "d", "l", "o", "r", "p", "n", "k", "u", "w", "e",
      "b", "f", "g", "h", "j", "t", "x", "y",  // emacs/nano/shell bindings
    ]);
    if (!terminalKeys.has(key)) {
      // Let the browser handle it (new tab, tab switch, etc.).
      e.stopImmediatePropagation();
      return;
    }
  }

  if (outputPaused) resumeOutput();
  if (term?.element && document.activeElement !== (term as any).textarea) {
    term.focus();
  }
}, { capture: true });

// Paste: intercept paste events and send content through bracketed paste
// mode so Claude Code sees it as a paste (showing "[Pasted text ...]")
// rather than as typed input.
document.addEventListener("paste", (e: ClipboardEvent) => {
  if (!term?.element || !ws || ws.readyState !== WebSocket.OPEN) return;
  const text = e.clipboardData?.getData("text");
  if (!text) return;
  e.preventDefault();
  // Bracketed paste: \x1b[200~ ... content ... \x1b[201~
  const payload = `\x1b[200~${text}\x1b[201~`;
  ws.send(textEncoder.encode(payload));
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
    if (!res.ok) return;
    const sessions: SessionInfo[] = await res.json();
    renderSessions(sessions);
  } catch (e) {
    console.error("Failed to fetch sessions:", e);
  }
}

let lastSessionsJson = "";

function renderSessions(sessions: SessionInfo[]): void {
  // Skip DOM rebuild if data and active session haven't changed.
  const json = JSON.stringify(sessions) + "|" + (currentSession ?? "");
  if (json === lastSessionsJson) return;
  lastSessionsJson = json;

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

      const safeStatus = ["running", "stopped"].includes(s.status) ? s.status : "stopped";
      li.innerHTML = `
        <span class="status-dot ${safeStatus}"></span>
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
  scrollAccum = 0;
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
    // Fallback reveal timer in case the scrollback_end marker never arrives.
    revealTimer = setTimeout(reveal, 350);
    checkVersion();
  };

  ws.onmessage = (e: MessageEvent) => {
    if (thisGeneration !== connectGeneration) return;

    // Text frames are control messages from the server.
    if (typeof e.data === "string") {
      try {
        const msg = JSON.parse(e.data);
        if (msg.type === "scrollback_end") {
          // Scrollback fully buffered. Reveal with a short grace period
          // for the resize repaint to arrive (sent after scrollback).
          if (revealTimer) { clearTimeout(revealTimer); revealTimer = null; }
          revealTimer = setTimeout(reveal, 50);
        }
      } catch { /* ignore malformed JSON */ }
      return;
    }

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
      ws.send(textEncoder.encode(data));
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
  ws.onerror = (e) => {
    console.error("WebSocket error:", e);
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
  banner.innerHTML = `Update available · <a href="javascript:void(0)" id="update-reload">reload</a>`;
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

// Focus terminal on click outside canvas (but not on buttons/sidebar items).
document.addEventListener("mousedown", (e: MouseEvent) => {
  if (!term?.element) return;
  if ((e.target as HTMLElement).closest("button, li")) return;
  term.focus();
});

