#!/usr/bin/env bash
set -euo pipefail

LAUNCHD_LABEL="com.gh6.daemon"
PLIST_SRC="$(dirname "$0")/com.gh6.daemon.plist"
PLIST_DST="$HOME/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
BIN_DIR="$HOME/.cargo/bin"
GH6_BIN="$BIN_DIR/gh6"
GH6D_BIN="$BIN_DIR/gh6d"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

usage() {
    echo "Usage: $0 [uninstall]"
    exit 1
}

install() {
    echo "=== Building gh6 & gh6d ==="
    cargo install --path . --force

    # Fix binary path in plist
    echo "=== Installing launchd plist ==="
    mkdir -p "$(dirname "$PLIST_DST")"
    sed "s|/Users/umoho/.cargo/bin/gh6d|$GH6D_BIN|" "$PLIST_SRC" > "$PLIST_DST"

    local domain="gui/$UID"
    launchctl bootout "$domain" "$PLIST_DST" 2>/dev/null || true
    launchctl bootstrap "$domain" "$PLIST_DST"

    echo -e "${GREEN}✓${NC} Installed: ${GH6_BIN}"
    echo -e "${GREEN}✓${NC} Installed: ${GH6D_BIN}"
    echo -e "${GREEN}✓${NC} launchd loaded: ${LAUNCHD_LABEL}"
    echo
    echo "Commands:"
    echo "  gh6 run     — start crawling"
    echo "  gh6 pause   — pause crawling"
    echo "  gh6 status  — view progress"
    echo "  launchctl bootout gui/\$UID $PLIST_DST  — stop daemon"
    echo "  launchctl bootstrap gui/\$UID $PLIST_DST  — start daemon"
}

uninstall() {
    echo "=== Stopping daemon ==="
    launchctl bootout "gui/$UID" "$PLIST_DST" 2>/dev/null || echo "(not loaded)"

    echo "=== Removing binaries ==="
    rm -f "$GH6_BIN" "$GH6D_BIN"

    echo "=== Removing plist ==="
    rm -f "$PLIST_DST"

    echo -e "${GREEN}✓${NC} Uninstalled."
}

case "${1:-install}" in
    install) install ;;
    uninstall) uninstall ;;
    *) usage ;;
esac
