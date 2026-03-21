#!/usr/bin/env bash
#
# Margatroid uninstaller.
#
# Stops services, removes binaries, systemd units, and optionally
# removes session data and the source repo.
#
set -euo pipefail

INSTALL_DIR="${MARGATROID_DIR:-$HOME/.margatroid}"
SYSTEMD_DIR="$HOME/.config/systemd/user"
CONFIG_DIR="$HOME/.config/margatroid"
SESSIONS_DIR="$HOME/sessions"

BLUE=$'\033[1;34m'
DIM=$'\033[2m'
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
# Remove config state
# ---------------------------------------------------------------------------

if [ -d "$CONFIG_DIR" ]; then
    info "Removing config ($CONFIG_DIR)"
    rm -rf "$CONFIG_DIR"
fi

# ---------------------------------------------------------------------------
# Remove source and binaries
# ---------------------------------------------------------------------------

if [ -d "$INSTALL_DIR" ]; then
    info "Removing $INSTALL_DIR (source + binaries)"
    rm -rf "$INSTALL_DIR"
fi

# ---------------------------------------------------------------------------
# Session data
# ---------------------------------------------------------------------------

if [ -d "$SESSIONS_DIR" ]; then
    echo
    printf '%sSession data remains at %s%s\n' "$DIM" "$SESSIONS_DIR" "$RESET"
    printf '%sRemove it manually if you no longer need it:%s\n' "$DIM" "$RESET"
    printf '%s  rm -rf %s%s\n' "$DIM" "$SESSIONS_DIR" "$RESET"
fi

echo
info "Margatroid uninstalled."
