#!/usr/bin/env bash
#
# Margatroid uninstaller.
#
# Stops services, removes systemd units, and deletes ~/.margatroid
# (source, binaries, state, and session data).
#
set -euo pipefail

INSTALL_DIR="${MARGATROID_DIR:-$HOME/.margatroid}"
SYSTEMD_DIR="$HOME/.config/systemd/user"

DIM=$'\033[2m'
BLUE=$'\033[1;34m'
RESET=$'\033[0m'

info() { printf '%s==>%s %s\n' "$BLUE" "$RESET" "$*"; }

# Run a command, showing its output in dim text. Suppresses output in non-TTY.
run_dim() {
    if [ -t 1 ]; then
        "$@" 2>&1 | while IFS= read -r line; do
            printf '%s    %s%s\n' "$DIM" "$line" "$RESET"
        done
    else
        "$@" >/dev/null 2>&1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Stop and disable services
# ---------------------------------------------------------------------------

info "Stopping services"
run_dim systemctl --user disable --now margatroid-update.timer || true
run_dim systemctl --user disable --now margatroid-daemon.service || true
run_dim systemctl --user disable --now margatroid-tmux.service || true

# Kill the tmux session if it's still around
run_dim tmux kill-session -t margatroid || true

# ---------------------------------------------------------------------------
# Remove systemd units
# ---------------------------------------------------------------------------

info "Removing systemd units"
rm -f "$SYSTEMD_DIR/margatroid-tmux.service"
rm -f "$SYSTEMD_DIR/margatroid-daemon.service"
rm -f "$SYSTEMD_DIR/margatroid-update.service"
rm -f "$SYSTEMD_DIR/margatroid-update.timer"
run_dim systemctl --user daemon-reload || true

# ---------------------------------------------------------------------------
# Remove everything
# ---------------------------------------------------------------------------

if [ -d "$INSTALL_DIR" ]; then
    info "Removing $INSTALL_DIR"
    rm -rf "$INSTALL_DIR"
fi

echo
info "Margatroid uninstalled."
