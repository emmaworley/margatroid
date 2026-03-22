# PTY Interceptor Plan

Replace the current tmux grouped session hack for web access with a relay
process that owns the PTY master, giving both tmux and web clients direct
access to session I/O.

## Current state

`session::launch()` calls `exec(podman/claude)`, handing PTY ownership to
tmux permanently. The web server works around this by creating temporary
grouped tmux sessions for host sessions and `podman attach` for container
sessions.

## Target architecture

```
tmux pane → margatroid-relay
              ├─ openpty() → master/slave
              ├─ fork() → child: exec(podman/claude) on slave
              ├─ relays stdin/stdout ↔ PTY master  (tmux sees a normal terminal)
              └─ Unix socket at ~/.margatroid/sessions/<name>/relay.sock
                   └─ web clients connect here (fan-out output, merge input)
```

The relay IS the tmux pane process. tmux is unaware anything changed.
Web clients connect via the Unix socket, bypassing tmux entirely.
Works identically for host and container sessions.

## Implementation steps

### 1. New binary: margatroid-relay

Create a `relay/` crate (bin). The relay process:

- Accepts the session command (podman or claude) as arguments
- Creates a PTY pair (`openpty`)
- Forks: child execs the session command on the slave
- Parent enters an event loop:
  - Reads stdin → writes to PTY master (tmux input)
  - Reads PTY master → writes to stdout (tmux output)
  - Accepts connections on a Unix socket
  - For each socket client: fan-out PTY output, merge client input
- On child exit (SIGCHLD): close socket, exit (tmux pane dies, triggers cleanup)
- On SIGTERM/SIGHUP: forward to child, wait, exit

### 2. Refactor session::launch()

Change `launch()` to exec into `margatroid-relay` instead of directly into
podman/claude:

```
Before: exec(podman run ... claude ...)
After:  exec(margatroid-relay podman run ... claude ...)

Before: exec(claude --name ...)
After:  exec(margatroid-relay claude --name ...)
```

The relay wraps whatever command `launch()` would have exec'd. This keeps
the change minimal — `launch()` still execs and never returns, just into
a different binary.

### 3. Unix socket protocol

Simple framed protocol over the socket:

- **Server → client (output):** raw terminal bytes, streamed as-is
- **Client → server (input):** raw terminal bytes, written to PTY master
- **Client → server (resize):** `\x00` + 4-byte little-endian cols/rows
  (the NUL prefix distinguishes resize from input since NUL is not valid
  terminal input in practice)
- **Initial handshake:** client sends cols/rows on connect. Server replays
  recent scrollback (ring buffer, ~64KB) so the client sees current state.

### 4. Update web crate

Replace all bridge modes with a single approach:

- For any session, connect to `~/.margatroid/sessions/<name>/relay.sock`
- Bridge Unix socket ↔ WebSocket
- No tmux commands, no podman attach, no host/container branching

### 5. Update install.sh / uninstall.sh

Add `margatroid-relay` to the binary list.

### 6. Update boot.rs

No changes needed — boot calls `tmux::new_window(name, &[tui_path, name, image])`
which runs the TUI, which calls `launch()`, which now execs into the relay.

### 7. Update cleanup

No changes needed — when the relay exits, the tmux pane dies, triggering
the existing cleanup hook. The relay exits when its child (podman/claude)
exits.

## Design decisions to make

- **Scrollback replay:** How much to buffer? 64KB ring buffer covers ~1000
  lines. Clients connecting mid-session need context.
- **Resize policy:** When multiple clients have different sizes, whose
  dimensions apply? Options: first client wins, smallest common size,
  tmux always wins (web clients adapt).
- **Max clients:** Cap concurrent socket connections to prevent resource
  exhaustion. 8-16 is plenty.
- **Relay crash recovery:** If the relay dies unexpectedly, the session
  dies (child gets SIGHUP). Acceptable — same as current behavior if the
  tmux pane process dies. Could add a watchdog later.
- **Fork helper integration:** The fork helper currently uses
  `tmux::capture_pane()` and `tmux::send_keys()` to detect the prompt
  and send `/remote-control`. With the relay owning the PTY, the helper
  could instead connect to the Unix socket — or the relay could subsume
  the helper's role entirely (detect prompt from PTY output, inject
  /remote-control directly). This would eliminate the fork helper.

## Risks

- The relay adds a process layer in the I/O path. Terminal latency should
  be negligible (< 1ms) but needs testing with heavy output (cargo build).
- Signal forwarding must be correct: SIGTERM, SIGINT, SIGWINCH all need
  to reach the child. SIGCHLD must be caught to detect child exit.
- The relay binary must be available before any session launches. Install
  order matters.
- All session launch paths (boot restore, TUI interactive, TUI direct,
  daemon /start) go through `session::launch()`, so one refactor covers
  all of them — but a bug there breaks everything.
