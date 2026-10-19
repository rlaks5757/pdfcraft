#!/usr/bin/env bash
# PPM 포크 웹 빌드: 한글 글꼴(NanumGothic, OFL-1.1)을 넣어 dist/web 을 만든다.
# 필요: Rust 1.95+ (기본 1.99.1), wasm32-unknown-unknown target, trunk 0.21+
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export CRAFT_FONTS_DIR="$root/ppm/fonts-ko"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.99.1}"
cd "$root/apps/pdfcraft-web"
trunk build --release
