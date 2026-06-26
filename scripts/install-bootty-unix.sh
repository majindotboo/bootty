#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Bootty"
BINARY_NAME="bootty"
DIST_DIR="${BOOTTY_DIST_DIR:-dist}"

./scripts/package-bootty-unix.sh

case "$(uname -s)" in
  Darwin)
    INSTALL_DIR="${BOOTTY_INSTALL_DIR:-/Applications}"
    APP_SOURCE="$DIST_DIR/$APP_NAME.app"
    APP_TARGET="$INSTALL_DIR/$APP_NAME.app"

    if [[ ! -d "$APP_SOURCE" ]]; then
      echo "packaged app not found at $APP_SOURCE" >&2
      exit 1
    fi

    rm -rf "$APP_TARGET"
    cp -R "$APP_SOURCE" "$APP_TARGET"
    echo "Installed $APP_TARGET"
    ;;
  Linux)
    PREFIX="${BOOTTY_INSTALL_PREFIX:-$HOME/.local}"
    ARCH="$(uname -m)"
    ROOT_DIR="$DIST_DIR/$APP_NAME-linux-$ARCH"

    if [[ ! -d "$ROOT_DIR" ]]; then
      echo "packaged app not found at $ROOT_DIR" >&2
      exit 1
    fi

    install -Dm755 "$ROOT_DIR/bin/$BINARY_NAME" "$PREFIX/bin/$BINARY_NAME"
    install -Dm644 "$ROOT_DIR/share/applications/dev.bootty.desktop" \
      "$PREFIX/share/applications/dev.bootty.desktop"
    install -Dm644 "$ROOT_DIR/share/icons/hicolor/256x256/apps/bootty.png" \
      "$PREFIX/share/icons/hicolor/256x256/apps/bootty.png"
    install -Dm644 "$ROOT_DIR/share/icons/hicolor/scalable/apps/bootty.svg" \
      "$PREFIX/share/icons/hicolor/scalable/apps/bootty.svg"

    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
      gtk-update-icon-cache -q "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
    fi

    echo "Installed $PREFIX/bin/$BINARY_NAME"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac
