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
set -euo pipefail

REPO_URL="https://github.com/emmaworley/margatroid.git"
INSTALL_DIR="${MARGATROID_DIR:-$HOME/.margatroid}"
BIN_DIR="$HOME/bin"
SYSTEMD_DIR="$HOME/.config/systemd/user"
BINARIES=(margatroid-boot margatroid-daemon margatroid-tui margatroid-cleanup)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

check_cmd() {
    command -v "$1" >/dev/null 2>&1 || error "Required command not found: $1"
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
    info "Fresh install — checking prerequisites"
    check_cmd git
    check_cmd cargo
    check_cmd podman
    check_cmd tmux

    info "Cloning repository to $INSTALL_DIR"
    git clone "$REPO_URL" "$INSTALL_DIR"
fi

# ---------------------------------------------------------------------------
# Update: pull latest changes, exit early if up to date
# ---------------------------------------------------------------------------

if [ "$MODE" = update ]; then
    cd "$INSTALL_DIR"
    git fetch origin main --quiet

    LOCAL=$(git rev-parse HEAD)
    REMOTE=$(git rev-parse origin/main)

    if [ "$LOCAL" = "$REMOTE" ]; then
        exit 0  # Already up to date — nothing to do
    fi

    info "Updating $(git log --oneline HEAD..origin/main | wc -l | tr -d ' ') new commit(s)"
    git pull --ff-only origin main --quiet
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

info "Building (release)"
cargo build --release --manifest-path "$INSTALL_DIR/Cargo.toml" --quiet

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
    cp "$src" "$dst"
done

info "Binaries installed to $BIN_DIR"

# ---------------------------------------------------------------------------
# Install systemd services and timer
# ---------------------------------------------------------------------------

mkdir -p "$SYSTEMD_DIR"

for f in "$INSTALL_DIR"/systemd/*.service "$INSTALL_DIR"/systemd/*.timer; do
    [ -f "$f" ] && cp "$f" "$SYSTEMD_DIR/"
done

systemctl --user daemon-reload

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
    info "Daemon restarted with updated binary"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

if [ "$MODE" = install ]; then
    info "Installation complete!"
    echo
    echo "  Binaries:  $BIN_DIR/margatroid-{boot,daemon,tui,cleanup}"
    echo "  Source:    $INSTALL_DIR"
    echo "  Services:  margatroid-tmux.service, margatroid-daemon.service"
    echo "  Updates:   margatroid-update.timer (hourly)"
    echo
    echo "  View status:   systemctl --user status margatroid-daemon.service"
    echo "  View logs:     journalctl --user -u margatroid-daemon.service -f"
    echo "  Manual update: $INSTALL_DIR/install.sh"
    echo
else
    NEW_VERSION=$(git -C "$INSTALL_DIR" log -1 --format='%h %s')
    info "Updated to: $NEW_VERSION"
fi
