#!/bin/bash

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

# Build the project
cargo build "${@:4}" || exit 1

# Strip the binary in release builds
if echo "$@" | grep -q "\-\-release"; then
  BINARY="$2/target/release/$1"
else
  BINARY="$2/target/debug/$1"
fi

strip "$BINARY" 2>/dev/null || true

# Copy the binary to the output location
cp "$BINARY" "$3" || exit 1
