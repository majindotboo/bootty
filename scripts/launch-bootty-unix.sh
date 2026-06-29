#!/usr/bin/env bash
set -euo pipefail

BINARY_NAME="bootty"
TARGET_ROOT="${CARGO_TARGET_DIR:-target}"
PROFILE="dynamic-release"

./scripts/build-bootty-unix.sh

BINARY_PATH="$TARGET_ROOT/$PROFILE/$BINARY_NAME"
if [[ ! -x "$BINARY_PATH" ]]; then
  echo "built binary not found at $BINARY_PATH" >&2
  exit 1
fi
LIBRARY_DIRS=("$TARGET_ROOT/$PROFILE/deps" "$(rustc --print target-libdir)")
case "$(uname -s)" in
  Darwin)
    DYLD_LIBRARY_PATH="$(IFS=:; echo "${LIBRARY_DIRS[*]}")${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
    export DYLD_LIBRARY_PATH
    ;;
  Linux)
    LD_LIBRARY_PATH="$(IFS=:; echo "${LIBRARY_DIRS[*]}")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export LD_LIBRARY_PATH
    ;;
esac

exec "$BINARY_PATH" "$@"
