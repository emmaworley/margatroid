#!/usr/bin/env bash
#
# Margatroid installer and updater.
#
# Fresh install:
#   curl -fsSL https://raw.githubusercontent.com/emmaworley/margatroid/main/install.sh | bash
#
# Update (runs automatically via systemd timer and on daemon start):
#   ~/.margatroid/install.sh
#

# Wrap everything in a function so bash reads the entire script into memory
# before executing. Without this, `git pull` can modify this file mid-run
# (bash reads scripts incrementally), causing partial old + new code execution.
main() {
set -euo pipefail

REPO_URL="https://github.com/emmaworley/margatroid.git"
INSTALL_DIR="${MARGATROID_DIR:-$HOME/.margatroid}"
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

# Run a command, showing only the last N lines of output in dim text while
# it runs, then clear those lines when done. Falls back to hidden output
# if not on a terminal.
run_visible() {
    local label="$1"
    shift

    info "$label"

    if [ ! -t 1 ]; then
        # Not a terminal (e.g. piped, systemd) — just run silently
        "$@" >/dev/null 2>&1
        return
    fi

    local tail_lines=5
    local tmpfile
    tmpfile=$(mktemp)

    # Run command, tee output to tmpfile, show last N lines live
    "$@" 2>&1 | while IFS= read -r line; do
        echo "$line" >> "$tmpfile"
        # Move up and clear previous tail lines, then print new tail
        local total
        total=$(wc -l < "$tmpfile" | tr -d ' ')
        local show=$tail_lines
        if [ "$total" -lt "$show" ]; then
            show=$total
        fi
        # Clear previous output (move up show lines and clear each)
        if [ "$total" -gt "$show" ]; then
            local clear_count=$show
        else
            local clear_count=$((total - 1))
        fi
        if [ "$clear_count" -gt 0 ]; then
            printf '\033[%dA' "$clear_count"
            for ((i=0; i<clear_count; i++)); do
                printf '\033[2K\n'
            done
            printf '\033[%dA' "$clear_count"
        fi
        tail -n "$show" "$tmpfile" | while IFS= read -r tline; do
            printf '%s    %s%s\n' "$DIM" "$tline" "$RESET"
        done
    done

    # Clear the tail lines after command completes
    local final_lines
    if [ -f "$tmpfile" ]; then
        final_lines=$(wc -l < "$tmpfile" | tr -d ' ')
    else
        final_lines=0
    fi
    local to_clear=$tail_lines
    if [ "$final_lines" -lt "$to_clear" ]; then
        to_clear=$final_lines
    fi
    if [ "$to_clear" -gt 0 ] && [ -t 1 ]; then
        printf '\033[%dA' "$to_clear"
        for ((i=0; i<to_clear; i++)); do
            printf '\033[2K\n'
        done
        printf '\033[%dA' "$to_clear"
    fi
    rm -f "$tmpfile"
}

# ---------------------------------------------------------------------------
# Detect mode: fresh install vs update
# ---------------------------------------------------------------------------

if [ -d "$INSTALL_DIR/.git" ]; then
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

    run_visible "Cloning repository" git clone "$REPO_URL" "$INSTALL_DIR"
fi

# ---------------------------------------------------------------------------
# Update: pull latest changes, exit early if up to date
# ---------------------------------------------------------------------------

if [ "$MODE" = update ]; then
    cd "$INSTALL_DIR"
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

run_visible "Building" cargo build --release --manifest-path "$INSTALL_DIR/Cargo.toml"

# ---------------------------------------------------------------------------
# Install binaries
# ---------------------------------------------------------------------------

mkdir -p "$BIN_DIR"

for bin in "${BINARIES[@]}"; do
    src="$INSTALL_DIR/target/release/$bin"
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

for f in "$INSTALL_DIR"/systemd/*.service "$INSTALL_DIR"/systemd/*.timer; do
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
# On update: restart the daemon so it picks up the new binary.
# This does NOT affect running container sessions — they live in tmux.
# ---------------------------------------------------------------------------

if [ "$MODE" = update ]; then
    systemctl --user restart margatroid-daemon.service 2>/dev/null || true
    NEW_VERSION=$(git -C "$INSTALL_DIR" log -1 --format='%h %s')
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
    echo "    ~/.margatroid/uninstall.sh"
    echo
fi

}
main "$@"
