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

BLUE=$'\033[1;34m'
RESET=$'\033[0m'

info() { printf '%s==>%s %s\n' "$BLUE" "$RESET" "$*"; }

# ---------------------------------------------------------------------------
# Stop and disable services
# ---------------------------------------------------------------------------

info "Stopping services"
systemctl --user disable --now margatroid-update.timer 2>/dev/null || true
systemctl --user disable --now margatroid-daemon.service 2>/dev/null || true
systemctl --user disable --now margatroid-tmux.service 2>/dev/null || true

# Kill the tmux session if it's still around
tmux kill-session -t margatroid 2>/dev/null || true

# ---------------------------------------------------------------------------
# Remove systemd units
# ---------------------------------------------------------------------------

info "Removing systemd units"
rm -f "$SYSTEMD_DIR/margatroid-tmux.service"
rm -f "$SYSTEMD_DIR/margatroid-daemon.service"
rm -f "$SYSTEMD_DIR/margatroid-update.service"
rm -f "$SYSTEMD_DIR/margatroid-update.timer"
systemctl --user daemon-reload 2>/dev/null || true

# ---------------------------------------------------------------------------
# Remove everything
# ---------------------------------------------------------------------------

if [ -d "$INSTALL_DIR" ]; then
    info "Removing $INSTALL_DIR"
    rm -rf "$INSTALL_DIR"
fi

echo
info "Margatroid uninstalled."
