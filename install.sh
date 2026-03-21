#!/usr/bin/env bash
#
# Margatroid installer and updater.
#
# Fresh install:
#   curl -fsSL https://raw.githubusercontent.com/emmaworley/margatroid/main/install.sh | bash
#
# Update (runs automatically via systemd timer and on daemon start):
#   ~/.margatroid/repo/install.sh
#

# Wrap everything in a function so bash reads the entire script into memory
# before executing. Without this, `git pull` can modify this file mid-run
# (bash reads scripts incrementally), causing partial old + new code execution.
main() {
set -euo pipefail

REPO_URL="https://github.com/emmaworley/margatroid.git"
INSTALL_DIR="${MARGATROID_DIR:-$HOME/.margatroid}"
REPO_DIR="$INSTALL_DIR/repo"
BIN_DIR="$INSTALL_DIR/bin"
SYSTEMD_DIR="$HOME/.config/systemd/user"
BINARIES=(margatroid-boot margatroid-daemon margatroid-tui margatroid-cleanup)

# ---------------------------------------------------------------------------
# Terminal helpers
# ---------------------------------------------------------------------------

DIM=$'\033[2m'
RESET=$'\033[0m'
BLUE=$'\033[1;34m'
YELLOW=$'\033[1;33m'
RED=$'\033[1;31m'
GREEN=$'\033[1;32m'

info()  { printf '%s==>%s %s\n' "$BLUE" "$RESET" "$*"; }
warn()  { printf '%s==>%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
error() { printf '%sERROR:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

check_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        error "Required command not found: $1"
    fi
}

# Run a command, showing its latest output line in dim text on a single
# overwritten line. Clears the status line when done. Falls back to
# hidden output if not on a terminal.
run_visible() {
    local label="$1"
    shift

    info "$label"

    if [ ! -t 1 ]; then
        "$@" >/dev/null 2>&1
        return
    fi

    local cols tmpfile line_count
    cols=$(tput cols 2>/dev/null || echo 80)
    tmpfile=$(mktemp)
    line_count=0

    "$@" 2>&1 | while IFS= read -r line; do
        echo "$line" >> "$tmpfile"
        local display="${line:0:$((cols - 4))}"
        # Overwrite previous status line (move up + clear), then print new one
        if [ "$line_count" -gt 0 ]; then
            printf '\033[A\033[2K'
        fi
        printf '%s    %s%s\n' "$DIM" "$display" "$RESET"
        line_count=$((line_count + 1))
    done

    # Clear the final status line if anything was printed.
    # The pipe subshell can't tell us, so check the tmpfile.
    if [ -s "$tmpfile" ]; then
        printf '\033[A\033[2K'
    fi
    rm -f "$tmpfile"
}

# ---------------------------------------------------------------------------
# Migrate from old layout: repo was cloned directly into $INSTALL_DIR
# ---------------------------------------------------------------------------

if [ -d "$INSTALL_DIR/.git" ] && [ ! -d "$REPO_DIR" ]; then
    info "Migrating repo to $REPO_DIR"
    # Move the git repo into a subdirectory, preserving sessions/state/bin
    tmp_repo="$INSTALL_DIR/.repo-migrate-$$"
    git clone "$INSTALL_DIR" "$tmp_repo" --quiet 2>/dev/null
    rm -rf "$INSTALL_DIR/.git"
    mv "$tmp_repo" "$REPO_DIR"
fi

# ---------------------------------------------------------------------------
# Detect mode: fresh install vs update
# ---------------------------------------------------------------------------

if [ -d "$REPO_DIR/.git" ]; then
    MODE=update
else
    MODE=install
fi

# ---------------------------------------------------------------------------
# Fresh install: check prerequisites
# ---------------------------------------------------------------------------

if [ "$MODE" = install ]; then
    info "Checking prerequisites"
    check_cmd git
    check_cmd cargo
    check_cmd podman
    check_cmd tmux

    # Check that claude is installed
    if ! command -v claude >/dev/null 2>&1; then
        error "Claude Code CLI not found. Install it first: https://docs.anthropic.com/en/docs/claude-code"
    fi

    # Check that claude credentials exist
    if [ ! -f "$HOME/.claude/.credentials.json" ]; then
        error "Claude Code credentials not found at ~/.claude/.credentials.json — run 'claude' and log in first"
    fi

    mkdir -p "$INSTALL_DIR"
    run_visible "Cloning repository" git clone "$REPO_URL" "$REPO_DIR"
fi

# ---------------------------------------------------------------------------
# Update: pull latest changes, exit early if up to date
# ---------------------------------------------------------------------------

if [ "$MODE" = update ]; then
    cd "$REPO_DIR"
    git fetch origin main --quiet 2>/dev/null || true

    LOCAL=$(git rev-parse HEAD)
    REMOTE=$(git rev-parse origin/main 2>/dev/null || echo "$LOCAL")

    if [ "$LOCAL" = "$REMOTE" ]; then
        exit 0  # Already up to date — nothing to do
    fi

    COMMIT_COUNT=$(git log --oneline HEAD..origin/main 2>/dev/null | wc -l | tr -d ' ')
    info "Pulling $COMMIT_COUNT new commit(s)"
    git pull --ff-only origin main --quiet 2>/dev/null
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

run_visible "Building" cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"

# ---------------------------------------------------------------------------
# Install binaries
# ---------------------------------------------------------------------------

mkdir -p "$BIN_DIR"

for bin in "${BINARIES[@]}"; do
    src="$REPO_DIR/target/release/$bin"
    dst="$BIN_DIR/$bin"
    if [ ! -f "$src" ]; then
        warn "Binary not found: $src (skipping)"
        continue
    fi
    # Use tmp+rename to atomically replace running binaries.
    # Direct cp fails with ETXTBSY if the binary is currently executing.
    tmp="$dst.tmp.$$"
    cp "$src" "$tmp"
    mv -f "$tmp" "$dst"
done

# ---------------------------------------------------------------------------
# Install systemd services and timer
# ---------------------------------------------------------------------------

mkdir -p "$SYSTEMD_DIR"

for f in "$REPO_DIR"/systemd/*.service "$REPO_DIR"/systemd/*.timer; do
    [ -f "$f" ] && cp "$f" "$SYSTEMD_DIR/"
done

systemctl --user daemon-reload 2>/dev/null || true

# ---------------------------------------------------------------------------
# Enable and start services (idempotent)
# ---------------------------------------------------------------------------

systemctl --user enable --now margatroid-tmux.service 2>/dev/null || true
systemctl --user enable --now margatroid-daemon.service 2>/dev/null || true
systemctl --user enable --now margatroid-update.timer 2>/dev/null || true

# ---------------------------------------------------------------------------
# On update: restart services so they pick up the new binaries.
# The boot service recreates the tmux session with the new TUI.
# Running container sessions are independent podman processes — they
# survive the tmux restart and get re-attached to new windows.
# ---------------------------------------------------------------------------

if [ "$MODE" = update ]; then
    systemctl --user restart margatroid-tmux.service 2>/dev/null || true
    systemctl --user restart margatroid-daemon.service 2>/dev/null || true
    NEW_VERSION=$(git -C "$REPO_DIR" log -1 --format='%h %s')
    info "Updated to: $NEW_VERSION"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

if [ "$MODE" = install ]; then
    echo
    printf '%s==>%s Installed!\n' "$GREEN" "$RESET"
    echo
    echo "  Attach to the session manager:"
    echo "    tmux attach -t margatroid"
    echo
    echo "  Open the web interface:"
    echo "    https://claude.ai/code"
    echo
    echo "  Uninstall:"
    echo "    ~/.margatroid/repo/uninstall.sh"
    echo
fi

}
main "$@"
