#!/usr/bin/env bash
# 在 macOS 上对 Windows 目标做 cargo check（仅编译检查，不链接、不运行）。
# 需要 rustup 的原生 aarch64 工具链已安装 x86_64-pc-windows-msvc 目标；
# Homebrew 的 cargo 不带该目标，所以这里直接调用 rustup 工具链里的二进制，并使用独立的 target 目录。
#
# 依赖（无 Windows SDK 的机器上编译 ring 的 C 代码需要）：
#   - Homebrew llvm（clang/llvm-ar）：MSVC 交叉编译与归档；
#   - scripts/msvc-shim：clang 自带头之外的最小系统头兜底（见该目录 README）。
# 环境里已有 CC/AR/CFLAGS 时沿用外部值，便于 CI 覆盖。
set -euo pipefail
cd "$(dirname "$0")/.."
TC="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"
if [[ ! -x "$TC/cargo" ]]; then
  echo "未找到 $TC/cargo，跳过 Windows 目标检查" >&2
  exit 0
fi
LLVM="${LLVM_BIN:-/opt/homebrew/opt/llvm/bin}"
if [[ -x "$LLVM/clang" ]]; then
  export CC="${CC:-$LLVM/clang}"
  export AR="${AR:-$LLVM/llvm-ar}"
  # clang 自带头（float.h/stdarg.h 等）与仓库内最小 shim 都以最低优先级追加
  CLANG_INC="$("$LLVM/clang" -print-resource-dir 2>/dev/null)/include"
  export CFLAGS="${CFLAGS:-} -idirafter $CLANG_INC -idirafter $(pwd)/scripts/msvc-shim"
fi
RUSTC="$TC/rustc" CARGO_TARGET_DIR=target/wincheck "$TC/cargo" check \
  --target x86_64-pc-windows-msvc -p helper-core -p codex-helper-credential -p codex-helper --all-targets "$@"
