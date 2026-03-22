# Margatroid

Manages containerized Claude Code sessions via tmux, Podman, and the Anthropic Bridge API. Provides a terminal UI for local management and a remote control interface accessible from claude.ai/code.

## Architecture

```
                                      ┌─────────────────┐
                                      │  claude.ai/code │
                                      │  (Web UI)       │
                                      └───────┬─────────┘
                                              │ WebSocket
                                              ▼
┌──────────┐   systemd    ┌──────────┐   SSE/REST   ┌────────────────────┐
│   boot   │─────────────►│  daemon  │◄────────────►│  Anthropic API     │
│          │              │          │              │  api.anthropic.com │
└──────────┘              └──────────┘              └────────────────────┘
     │                         │
     │ tmux                    │ tmux new-window
     ▼                         ▼
┌──────────────────────────────────────────────────┐
│            tmux session "margatroid"             │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐          │
│  │ _Session │ │ session1 │ │ session2 │ ...      │
│  │ Manager  │ │ (podman) │ │ (podman) │          │
│  │  (TUI)   │ │          │ │          │          │
│  └──────────┘ └────┬─────┘ └────┬─────┘          │
│                    │            │                │
│              pane-died hook → cleanup            │
└──────────────────────────────────────────────────┘
```

## Workspace Crates

| Crate | Type | Purpose |
|-------|------|---------|
| **margatroid** | lib | Core session, container, and tmux management |
| **bridge** | lib | HTTP/SSE client for the Anthropic Bridge API |
| **boot** | bin | Systemd service: creates tmux session, restores saved sessions |
| **daemon** | bin | Systemd service: bridge API worker for remote control via claude.ai/code |
| **tui** | bin | Terminal UI for browsing and managing sessions |
| **cleanup** | bin | Tmux pane-died hook handler: stops containers, deregisters sessions |
| **relay** | bin | PTY interceptor: owns session PTY, exposes Unix socket for web access |
| **web** | bin | Web server: ghostty-web frontend + WebSocket-to-relay bridge |

### Dependency Graph

```
margatroid (lib)            bridge (lib)
     │                           │
     ├── boot                    │
     ├── tui                     │
     ├── cleanup                 │
     ├── daemon ─────────────────┘
     └── web
relay (standalone)
```

## Prerequisites

- **Rust** (stable toolchain)
- **Podman** (rootless, for container management)
- **tmux** (for session multiplexing)
- **Git**
- **Claude Code** credentials at `~/.claude/.credentials.json` and `~/.claude.json`

## Installation

```sh
curl -fsSL https://raw.githubusercontent.com/emmaworley/margatroid/main/install.sh | bash
```

This clones the repo to `~/.margatroid`, builds from source, and enables systemd user services. Everything lives under `~/.margatroid`.

### What gets installed

```
~/.margatroid/bin/margatroid-{boot,daemon,tui,cleanup}    # Binaries
~/.config/systemd/user/margatroid-tmux.service             # Session manager service
~/.config/systemd/user/margatroid-daemon.service           # Bridge daemon service
~/.config/systemd/user/margatroid-update.{service,timer}   # Auto-update
```

### Updates

Updates happen automatically:
- **On daemon start/restart** — pulls and rebuilds if there are new commits
- **Hourly timer** — checks for updates in the background, rebuilds and restarts the daemon if needed

Running sessions are not disrupted by updates. Container sessions live in tmux and are independent of the daemon process.

To update manually:

```sh
~/.margatroid/repo/install.sh
```

### Uninstall

```sh
~/.margatroid/repo/uninstall.sh
```

Stops services, removes systemd units, and deletes `~/.margatroid` (source, binaries, state, and session data).

## Building from source

```sh
cargo build --release
```

Binaries are produced in `target/release/`:
- `margatroid-boot`
- `margatroid-daemon`
- `margatroid-tui`
- `margatroid-cleanup`

## Session Lifecycle

1. **Boot** creates the shared tmux session and restores any previously saved sessions from `~/.margatroid/state/sessions.json`.

2. **Sessions are created** via the TUI (`/start <name> [image]`) or the remote control web UI. Each session gets:
   - A working directory at `~/.margatroid/sessions/<name>/`
   - A Podman container running the specified image
   - A tmux window within the shared `margatroid` session
   - Claude Code running inside the container with `/remote-control` mode

3. **The daemon** registers as a bridge environment with the Anthropic API, creates a "Session Manager" session visible at claude.ai/code, and responds to slash commands (`/list`, `/start`, `/stop`, etc.) from the web UI.

4. **On pane death** (container exit, crash, or manual close), the tmux `pane-died` hook invokes the cleanup binary, which stops the container and deregisters the session from state.

5. **On shutdown**, the daemon archives its session and deregisters the environment. The boot service exits when the tmux session is destroyed.

## Remote Control Commands

The daemon exposes these commands via the claude.ai/code web interface:

| Command | Description |
|---------|-------------|
| `/list` | List all sessions |
| `/start <name> [image]` | Start a new session (default image: ubuntu) |
| `/stop <name>` | Stop a running session |
| `/restart <name>` | Restart a session |
| `/delete <name> [--data]` | Delete a session (optionally remove data) |
| `/info <name>` | Show session details |
| `/help` | Show command help |

## Configuration

### Credential Files

```
~/.claude/.credentials.json    # OAuth access token
~/.claude.json                 # Organization UUID, trust settings
```

### Persistent State

```
~/.margatroid/state/
  sessions.json                # Active sessions (name → image)
```

### Session Data

```
~/.margatroid/sessions/<name>/ # Per-session working directory (mounted into container)
```

## Bridge Protocol

The daemon implements the Anthropic Bridge protocol for remote control. See `bridge/SPEC.md` for the full reverse-engineered specification, including:

- Environment registration and work polling
- SSE event stream for receiving user messages
- Worker event API for sending responses
- Control request/response handling (initialize, set_model)
- Session archival and environment deregistration
- Critical delivery report ordering requirements

## Systemd Services

### margatroid-tmux.service

Creates the shared tmux session and restores saved sessions. Runs as a user service.

### margatroid-daemon.service

Polls the bridge API for work and manages remote control sessions. Depends on `margatroid-tmux.service`. Checks for updates on start. Automatically restarts on failure.

### margatroid-update.timer

Checks for updates hourly. On new commits: pulls, rebuilds, and restarts the daemon. Running sessions are unaffected.

## Development

Run with debug logging:

```sh
RUST_LOG=debug cargo run --bin margatroid-daemon
```

Run the TUI interactively:

```sh
cargo run --bin margatroid-tui
```

Launch a session directly (bypasses TUI):

```sh
cargo run --bin margatroid-tui -- <session-name> <image>
```
