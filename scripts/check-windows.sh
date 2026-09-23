#!/usr/bin/env bash
# 在 macOS 上对 Windows 目标做 cargo check（仅编译检查，不链接、不运行）。
# 需要 rustup 的原生 aarch64 工具链已安装 x86_64-pc-windows-msvc 目标；
# Homebrew 的 cargo 不带该目标，所以这里直接调用 rustup 工具链里的二进制，并使用独立的 target 目录。
set -euo pipefail
cd "$(dirname "$0")/.."
TC="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"
if [[ ! -x "$TC/cargo" ]]; then
  echo "未找到 $TC/cargo，跳过 Windows 目标检查" >&2
  exit 0
fi
RUSTC="$TC/rustc" CARGO_TARGET_DIR=target/wincheck "$TC/cargo" check \
  --target x86_64-pc-windows-msvc -p helper-core -p codex-helper-credential --all-targets "$@"
