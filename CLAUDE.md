# Development Instructions

## Testing
- Always run tests with: `cargo +nightly careful test` (not `cargo test`)
- Always run `shellcheck` on shell scripts after modifying them
- Always run `cargo clippy --all-targets` after changes
- Frontend tests: `cd web/frontend && npx playwright test`
- Lint frontend: `cd web/frontend && pnpm lint`
- After building changed binaries, install them: `cp target/debug/<bin> ~/.margatroid/bin/<bin>` (tmp+mv pattern)
- Existing sessions must be restarted to pick up installed binary changes
- Ask for review before pushing changes

## Installer
- Keep `install.sh` (installer) and `uninstall.sh` (uninstaller) up-to-date when changing:
  - Binary names, crate names, or systemd service names
  - File paths (bin dir, config dir, systemd dir)
  - Any new files that get installed to the system
- `install.sh` is wrapped in `main()` so bash reads it fully before executing — git pull can't corrupt it mid-run
- Binary replacement uses tmp+mv (not cp) to avoid ETXTBSY on running binaries
- The installer restarts both margatroid-tmux and margatroid-daemon on update

## Architecture gotchas

### Two session modes: container and host
- Container sessions (`image != "host"`): podman container, isolated home at `/home/<name>`, per-session `.claude.json`, ro credentials mount
- Host sessions (`image == "host"`): Claude Code runs directly on host, uses host `~/.claude.json` for trust
- `setup()`, `stop()`, and `delete()` must handle both modes — check `state.image == "host"` to branch

### Container mount layout
- Session dir → `/home/<name>` (container's $HOME, rw)
- Host `~/.claude/.credentials.json` → `/home/<name>/.claude/.credentials.json` (ro)
- Claude binary dir → same path (ro)
- No rw mount of host `~/.claude` or `~/.claude.json` — containers are isolated

### Per-session .claude.json (container mode only)
- Seeded by `setup_session` with: trust for `/home/<name>`, `remoteDialogSeen`, `hasCompletedOnboarding`, org UUID
- Lives at `~/.margatroid/sessions/<name>/.claude.json` on host, becomes `/home/<name>/.claude.json` in container
- Org UUID is read from host `~/.claude.json` at setup time

### Host .claude.json modifications
- `remoteDialogSeen: true` — always set (skips /remote-control confirmation)
- Trust entry for session dir — only set for host-mode sessions (container sessions trust via per-session config)
- Writes are atomic (tmp+rename)

### Stopping sessions
- Container: `podman stop -t 10` + `podman rm` — container gets SIGTERM, Claude Code deregisters
- Host: send `/exit` via tmux, wait 10s for clean exit, fall back to `q`
- Service stop: `ExecStop` runs `podman stop` on all margatroid-* containers before killing tmux
- If Claude Code doesn't deregister, the remote control session is orphaned on claude.ai/code

### The remote-control helper (fork_helper)
- Forked process that waits for Claude Code's prompt, optionally injects a resume message, then sends `/remote-control`
- Closes FDs 3..1024 after fork to avoid leaking lock files
- The `/remote-control` confirmation is pre-accepted via `remoteDialogSeen` in config — no screen-scraping needed

### Naming
- Project name is `margatroid` everywhere — not `orchestrator` or `claude-`
- Container names: `margatroid-<session>`
- Tmux session: `margatroid`
- Config dir: `~/.margatroid/`
- References to "Claude Code" (the product), `~/.claude/`, `.claude.json`, and the `claude` binary are NOT project names — don't rename those

### Container security model
- Containers run as root INSIDE (can apt install, pip install, etc.)
- Podman rootless user namespace maps container UID 0 → host user's UID
- Files on mounted volumes are owned by the host user despite appearing as root inside
- This is the standard distrobox/toolbox pattern — container escape only grants host-user privileges

### Things that have broken before
- Hardcoded UIDs (1001) in podman — use `getuid()`/`getgid()` instead
- Hardcoded `/home/claude` paths in container — use actual `home_dir()` or `/home/<name>`
- SELinux blocking bind-mounted binaries — use `--security-opt label=disable` and mount dirs not files
- `--entrypoint` with catatonit — pass command after image name instead
- Alternate screen in TUI — don't use it, the TUI is permanent in a detached tmux session
- install.sh modifying itself via git pull mid-run — the `main()` wrapper prevents this
- JWT refresh timer not resetting after refresh — must update both `jwt_obtained_at` and `jwt_expires_in`
- Session name path traversal — validate names (reject `/`, `..`, `\0`, empty) before filesystem ops
- Post-fork panic UB — prepare CStrings before fork, use libc functions in child, never panic
- Unix socket injection — chmod relay.sock to 0600 after bind
- Task leaks in tokio::select! — abort both tasks after select completes
- Forked children inheriting FDs — close 3..1024 before exec
- Running services use installed binaries, not build output — always install after building

### PTY relay (margatroid-relay)
- Every session launches through `margatroid-relay <name> <command> [args...]`
- The relay owns the PTY master, forks the session command on the slave
- Relays stdin/stdout for tmux (transparent — tmux doesn't know it's there)
- Listens on `~/.margatroid/sessions/<name>/relay.sock` for web clients
- 64KB ring buffer replays scrollback to newly connecting clients
- Broadcast fan-out: all web clients see the same output
- Resize from web clients: `\x00` + 2-byte LE cols + 2-byte LE rows
- SIGWINCH from tmux is forwarded to the inner PTY

### Web interface (margatroid-web)
- Serves the ghostty-web frontend on port 8080
- Sessions connect via Unix socket to the relay — no tmux involvement
- Manager (`_manager`) spawns a fresh TUI in a PTY (no relay)
- Frontend: ghostty-web (WASM terminal), pnpm + parcel build
- FitAddon auto-sizes terminal to browser window
- URL fragment (`#session-name`) persists and restores session on refresh

### Frontend build
- Source: `web/frontend/src/` (index.html, main.ts, style.css)
- Build: `cd web/frontend && pnpm install && pnpm build`
- Output: `web/static/dist/` (served by margatroid-web)
- Must copy `node_modules/ghostty-web/ghostty-vt.wasm` to `web/static/dist/` after build

## File layout
```
~/.margatroid/                    # Everything lives here
  repo/                           # Git clone (source, Cargo.toml, target/)
  bin/                            # Installed binaries
    static/                       # Web frontend dist (installed by install.sh)
  sessions/<name>/                # Per-session working directories
    .claude.json                  # Per-session config (container mode)
    .claude/                      # Mount target for credentials
    relay.sock                    # Unix socket for web access (created by relay)
    CLAUDE.md                     # Session instructions
  state/
    sessions.json                 # Active sessions (name → image)
    sessions.json.lock            # flock file (O_CLOEXEC)
```

<!-- margatroid:start -->
## Margatroid Session: margatroid-dev

**Host session** — running directly on the host machine with host user permissions.

Working directory: `/home/margatroid/.margatroid/sessions/margatroid-dev`
All files here persist across session restarts.

This session is managed by margatroid. It may be stopped and restarted
automatically (e.g. during updates). In-memory state (running processes,
environment variables) is not preserved across restarts.
<!-- margatroid:end -->
