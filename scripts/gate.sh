#!/usr/bin/env bash
# 阶段门禁：格式、clippy、全部测试、Windows 目标编译检查、前端类型检查。
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== cargo fmt --check";   cargo fmt --all --check
echo "== cargo clippy";        cargo clippy --workspace --all-targets -- -D warnings
echo "== cargo test";          cargo test --workspace
echo "== windows check";       scripts/check-windows.sh
echo "== tsc";                 npm --prefix apps/desktop run check
echo "== frontend test";       npm --prefix apps/desktop test
echo "== frontend build";      npm --prefix apps/desktop run build
echo "== gate passed"
