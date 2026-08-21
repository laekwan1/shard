#!/bin/bash
# Build the Rust core for iPhone + simulator and wrap it as an xcframework the
# app links. Runs on macOS (GitHub's runner or a local Mac); needs the iOS Rust
# targets installed (`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`).
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "== building libshard_mobile for device + simulator =="
cargo build -p shard-mobile --release --target aarch64-apple-ios
cargo build -p shard-mobile --release --target aarch64-apple-ios-sim

HEADERS="ios/build/headers"
OUT="ios/build/ShardCore.xcframework"
rm -rf "$OUT" "$HEADERS"
mkdir -p "$HEADERS"
cp ios/Shard/Sources/ShardCore.h "$HEADERS/"

echo "== assembling $OUT =="
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libshard_mobile.a -headers "$HEADERS" \
  -library target/aarch64-apple-ios-sim/release/libshard_mobile.a -headers "$HEADERS" \
  -output "$OUT"

echo "== done =="
ls -la "$OUT"
