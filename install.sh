#!/usr/bin/env bash
#
# Install / uninstall sh_web_search_tool_rs as a systemd service.
#
#   sudo ./install.sh install     # create user, copy binary, enable service
#   sudo ./install.sh uninstall   # stop service, remove unit and binary
#
set -euo pipefail

# Resolve build artifacts relative to the repository, not the caller's cwd.
cd "$(dirname "$0")"

SERVICE_NAME="web-search"
BIN_NAME="web-search"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
UNIT_SRC="web-search.service"
UNIT_PATH="/etc/systemd/system/${SERVICE_NAME}.service"
DOC_DIR="/usr/local/share/doc/${SERVICE_NAME}"
RUN_USER="websearch"
MUSL_TARGET="x86_64-unknown-linux-musl"
STATIC_BIN="target/${MUSL_TARGET}/release/sh_web_search_tool_rs"
RELEASE_BIN="target/release/sh_web_search_tool_rs"

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        die "must be run as root: sudo ./install.sh $1"
    fi
}

find_binary() {
    # A static musl build is preferred: it has no external dependencies.
    if [ -x "$STATIC_BIN" ]; then
        echo "$STATIC_BIN"
    elif [ -x "$RELEASE_BIN" ]; then
        echo "$RELEASE_BIN"
    else
        die "binary not found — run 'make build' (or 'make build-static') first"
    fi
}

create_user() {
    if id -u "$RUN_USER" >/dev/null 2>&1; then
        log "user '$RUN_USER' already exists"
        return
    fi
    log "creating system user '$RUN_USER'"
    if command -v useradd >/dev/null 2>&1; then
        useradd --system --no-create-home --shell /usr/sbin/nologin "$RUN_USER"
    elif command -v adduser >/dev/null 2>&1; then
        adduser --system --no-create-home --shell /usr/sbin/nologin "$RUN_USER"
    else
        die "neither useradd nor adduser is available"
    fi
}

install_service() {
    require_root install
    local binary
    binary="$(find_binary)"
    log "installing $binary"

    create_user

    install -d "$INSTALL_DIR"
    install -m 0755 "$binary" "$INSTALL_DIR/$BIN_NAME"

    [ -f "$UNIT_SRC" ] || die "unit file $UNIT_SRC not found"
    install -m 0644 "$UNIT_SRC" "$UNIT_PATH"

    if [ -f README.md ]; then
        install -d "$DOC_DIR"
        install -m 0644 README.md "$DOC_DIR/README.md"
    fi

    log "reloading systemd and enabling $SERVICE_NAME"
    systemctl daemon-reload
    systemctl enable "$SERVICE_NAME"
    systemctl restart "$SERVICE_NAME"
    systemctl --no-pager status "$SERVICE_NAME" || true

    cat <<EOF

Installed:
  binary   $INSTALL_DIR/$BIN_NAME
  unit     $UNIT_PATH
  user     $RUN_USER

Check:
  systemctl status $SERVICE_NAME
  journalctl -u $SERVICE_NAME -f
  curl -s localhost:3333/health
EOF
}

uninstall_service() {
    require_root uninstall
    log "stopping and disabling $SERVICE_NAME"
    systemctl disable --now "$SERVICE_NAME" 2>/dev/null || true
    rm -f "$UNIT_PATH"
    rm -f "$INSTALL_DIR/$BIN_NAME"
    rm -rf "$DOC_DIR"
    systemctl daemon-reload
    log "uninstalled (user '$RUN_USER' left in place)"
}

case "${1:-install}" in
    install)   install_service ;;
    uninstall) uninstall_service ;;
    *) die "unknown command: $1 (expected 'install' or 'uninstall')" ;;
esac
