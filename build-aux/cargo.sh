#!/bin/bash

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

# Parse --target-dir from args
TARGET_DIR=""
for arg in "$@"; do
  if [ -n "$PREV_IS_TARGET_DIR" ]; then
    TARGET_DIR="$arg"
    break
  fi
  if [ "$arg" = "--target-dir" ]; then
    PREV_IS_TARGET_DIR=1
  fi
done

# Build the project
cargo build "${@:4}" || exit 1

# Determine binary location
if [ -n "$TARGET_DIR" ]; then
  if echo "$@" | grep -q "\-\-release"; then
    BINARY="$TARGET_DIR/release/$1"
  else
    BINARY="$TARGET_DIR/debug/$1"
  fi
else
  if echo "$@" | grep -q "\-\-release"; then
    BINARY="$2/target/release/$1"
  else
    BINARY="$2/target/debug/$1"
  fi
fi

# Strip the binary in release builds (preserve debuginfo for Flatpak/Flathub unless explicitly requested)
if [ "${CARGO_STRIP:-0}" = "1" ]; then
  strip "$BINARY" 2>/dev/null || true
fi

# Copy the binary to the output location
cp "$BINARY" "$3" || exit 1
