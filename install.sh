#!/usr/bin/env bash
# Installer for pvz2-save-sync.
#
# Downloads the latest release binary from GitHub and installs it.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/xamionex/pvz2-save-sync/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/xamionex/pvz2-save-sync/main/install.sh | sh -s -- --user
#
# Root install (default, preferred):
#   - binary goes to /usr/local/bin/pvz2-save-sync
#   - re-runs itself with sudo when not already root
#
# User install (--user):
#   - binary goes to ~/.local/bin/pvz2-save-sync
#
# The app needs adb (Android platform-tools) on PATH at runtime; the installer warns when it is missing.
set -eu

REPO="xamionex/pvz2-save-sync"
BIN_NAME="pvz2-save-sync"
# Overridable for testing and forks.
API="${PVZ2_SAVE_SYNC_API:-https://api.github.com/repos/$REPO/releases/latest}"
RAW_URL="${PVZ2_SAVE_SYNC_RAW:-https://raw.githubusercontent.com/$REPO/main}"

info()  { printf '%s\n' "==> $*"; }
warn()  { printf '%s\n' "!! $*" >&2; }
die()   { printf '%s\n' "!! $*" >&2; exit 1; }

# The user the install is for. For root installs this is the invoking user, not root,
# so the conflict check targets the right home directory.
if [ "$(id -u)" -eq 0 ]; then
    TARGET_USER="${SUDO_USER:-}"
    [ -n "$TARGET_USER" ] || die "running as root without SUDO_USER; run as a normal user instead"
else
    TARGET_USER="$(id -un)"
fi

# Resolve the target user's home directory without relying on $HOME, which sudo may or may not preserve.
TARGET_HOME="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
[ -n "$TARGET_HOME" ] || die "could not resolve home directory for user $TARGET_USER"

# Ask a yes/no question on the terminal.
# The script is usually piped from curl, so stdin is the script itself and cannot be read: the question goes to /dev/tty.
ask_remove() {
    printf 'Remove %s? [y/N] ' "$1" >/dev/tty
    answer=""
    read -r answer </dev/tty || true
    case "$answer" in
        [Yy]*) return 0 ;;
        *) return 1 ;;
    esac
}

# The same binary installed in the other location can shadow this one, depending on PATH order.
# Reports it, then asks whether to remove it. Never fatal: the install itself has already succeeded.
check_conflicting_install() {
    if [ "$MODE" = "root" ]; then
        conflict="$TARGET_HOME/.local/bin/$BIN_NAME"
    else
        conflict="/usr/local/bin/$BIN_NAME"
    fi
    [ -e "$conflict" ] || return 0

    warn "another $BIN_NAME is installed at $conflict (this install is $BIN_DIR/$BIN_NAME)"
    # A readability test on /dev/tty passes in processes without a controlling terminal, so try to open it.
    if ! (exec 3</dev/tty) 2>/dev/null; then
        info "no terminal to ask on; remove the other copy if you want only one install"
        return 0
    fi
    if ! ask_remove "$conflict"; then
        info "keeping $conflict; make sure $BIN_DIR comes first in PATH"
        return 0
    fi
    if [ "$MODE" = "root" ]; then
        if rm -f "$conflict"; then
            info "removed $conflict"
        else
            warn "could not remove $conflict"
        fi
    elif command -v sudo >/dev/null 2>&1 && sudo rm -f "$conflict"; then
        info "removed $conflict"
    else
        warn "could not remove $conflict; run: sudo rm -f $conflict"
    fi
}

MODE="root"
if [ "${1:-}" = "--user" ]; then
    MODE="user"
elif [ "${1:-}" = "--root" ]; then
    # Internal mode used by the sudo re-run below.
    MODE="root"
elif [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    cat <<EOF
Usage: curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sh [-- --user]

Installs the pvz2-save-sync binary.

Root install (default, preferred):
  binary at /usr/local/bin/$BIN_NAME.
  Prompts for sudo when needed.

User install (--user):
  binary at ~/.local/bin/$BIN_NAME.

An existing copy in the other location is reported, and removed when you confirm, so PATH order cannot pick a stale one.
The app needs adb (Android platform-tools) on PATH at runtime.
EOF
    exit 0
elif [ -n "${1:-}" ]; then
    die "unknown argument: $1 (use --user for a user install, --help for usage)"
fi

# Root installs re-run themselves with sudo so the binary can be written to /usr/local/bin.
# The script is re-downloaded inside the sudo shell because $0 is not a usable path when the script is piped from curl.
if [ "$MODE" = "root" ] && [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        info "root install requested, re-running with sudo"
        exec sudo TARGET_USER="$TARGET_USER" TARGET_HOME="$TARGET_HOME" \
            PVZ2_SAVE_SYNC_API="$API" PVZ2_SAVE_SYNC_RAW="$RAW_URL" \
            bash -c "curl -fsSL '$RAW_URL/install.sh' | bash -s -- --root"
    fi
    die "root install requested but sudo is not available; use --user instead"
fi

if [ "$MODE" = "root" ]; then
    BIN_DIR="/usr/local/bin"
else
    BIN_DIR="$TARGET_HOME/.local/bin"
fi

command -v curl >/dev/null 2>&1 || die "curl is required but not installed"
command -v mktemp >/dev/null 2>&1 || die "mktemp is required but not installed"

info "fetching latest release info for $REPO"
RELEASE_JSON="$(curl -fsSL "$API")" || die "failed to reach the GitHub API"

ASSET_URL="$(printf '%s' "$RELEASE_JSON" | sed -n 's/.*"browser_download_url": *"\([^"]*\/pvz2-save-sync\)".*/\1/p' | head -n 1)"
[ -n "$ASSET_URL" ] || die "no release asset named '$BIN_NAME' found; create a release first (see README)"

TAG="$(printf '%s' "$RELEASE_JSON" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
info "downloading $BIN_NAME ${TAG:+($TAG)}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL -o "$TMP_DIR/$BIN_NAME" "$ASSET_URL" || die "download failed"

# Sanity check: the asset must be an executable ELF binary.
head -c 4 "$TMP_DIR/$BIN_NAME" | grep -q "$(printf '\177ELF')" || die "downloaded file is not an ELF binary"

chmod 755 "$TMP_DIR/$BIN_NAME"

info "installing binary to $BIN_DIR/$BIN_NAME"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP_DIR/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

check_conflicting_install

if ! command -v adb >/dev/null 2>&1; then
    warn "adb was not found on PATH"
    warn "install Android platform-tools (adb) before using the app, then run it again"
fi

info "done"
if [ "$MODE" = "root" ]; then
    info "installed for user $TARGET_USER at $BIN_DIR/$BIN_NAME"
else
    info "installed at $BIN_DIR/$BIN_NAME"
    info "make sure $BIN_DIR is on your PATH"
fi
