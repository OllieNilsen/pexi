#!/usr/bin/env sh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWIFT_SOURCE="$ROOT_DIR/avf_runner.swift"
RUNNER_BIN="$ROOT_DIR/avf_runner"
ENTITLEMENTS="$ROOT_DIR/entitlements.plist"

if [ ! -f "$SWIFT_SOURCE" ]; then
  echo "Missing Swift source: $SWIFT_SOURCE" >&2
  exit 1
fi

if [ ! -f "$ENTITLEMENTS" ]; then
  echo "Missing entitlements: $ENTITLEMENTS" >&2
  exit 1
fi

echo "Compiling avf_runner..."
swiftc "$SWIFT_SOURCE" -o "$RUNNER_BIN"

echo "Codesigning avf_runner..."
codesign -s - --entitlements "$ENTITLEMENTS" --force "$RUNNER_BIN"

echo "avf_runner ready: $RUNNER_BIN"
