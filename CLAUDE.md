# Development Instructions

## Testing
- Always run tests with: `cargo +nightly careful test` (not `cargo test`)
- Always run `shellcheck` on shell scripts after modifying them
- Always run `cargo clippy --all-targets` after changes
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

### Things that have broken before
- Hardcoded UIDs (1001) in podman — use `getuid()`/`getgid()` instead
- Hardcoded `/home/claude` paths in container — use actual `home_dir()` or `/home/<name>`
- SELinux blocking bind-mounted binaries — use `--security-opt label=disable` and mount dirs not files
- `--entrypoint` with catatonit — pass command after image name instead
- Alternate screen in TUI — don't use it, the TUI is permanent in a detached tmux session
- install.sh modifying itself via git pull mid-run — the `main()` wrapper prevents this
- JWT refresh timer not resetting after refresh — must update both `jwt_obtained_at` and `jwt_expires_in`

## File layout
```
~/.margatroid/                    # Everything lives here
  bin/                            # Installed binaries
  sessions/<name>/                # Per-session working directories
    .claude.json                  # Per-session config (container mode)
    .claude/                      # Mount target for credentials
    CLAUDE.md                     # Session instructions
  state/
    sessions.json                 # Active sessions (name → image)
    sessions.json.lock            # flock file (O_CLOEXEC)
  (source repo, target/, etc.)
```
