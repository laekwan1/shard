#!/bin/bash
# Build the Rust core for iPhone + simulator and wrap it as an xcframework the
# app links. Runs on macOS (GitHub's runner or a local Mac); needs the iOS Rust
# targets installed (`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`).
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "== building libshard_mobile for device + simulator =="
# --features resign: iOS 자체 서명 엔진(①②③④⑤)의 C ABI를 staticlib에 포함(resign_ffi.rs).
# `cargo rustc --crate-type staticlib`로 **staticlib만** 낸다. shard-mobile은 cdylib도 crate-type에
# 두지만(Android JNI .so용) iOS는 .a만 쓴다. `cargo build`는 cdylib까지 빌드하며 그걸 **링크**하는데,
# 이제 벤더된 zsign(C++)이 OpenSSL 심볼을 참조하고 그 심볼은 최종 앱 링크(OpenSSL.xcframework)에서만
# 풀려서, cdylib 링크가 "Undefined symbols(OpenSSL)"로 실패했다. staticlib은 아카이브라 링크가 없어
# 미해결 심볼이 정상이고(앱에서 OpenSSL.xcframework로 해소), iOS엔 이 .a만 필요하다.
cargo rustc -p shard-mobile --release --features resign --target aarch64-apple-ios --lib --crate-type staticlib
cargo rustc -p shard-mobile --release --features resign --target aarch64-apple-ios-sim --lib --crate-type staticlib

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
