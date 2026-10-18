#!/usr/bin/env bash
# 构建 macOS universal（arm64 + x86_64）dmg，本机与 CI（release.yml 的 macos-release job）共用。
# 无参数；产物为 ad-hoc 签名（tauri.macos.conf.json 的 signingIdentity "-"）。
# 提供环境变量 TAURI_SIGNING_PRIVATE_KEY 时额外产出升级产物（.app.tar.gz + .sig），
# 未提供时降级为不生成升级产物（本地开发构建不受影响）。
#
# 前置条件：
# - macOS，Xcode 命令行工具（lipo、codesign、hdiutil）
# - rustup 原生工具链 stable-aarch64-apple-darwin，已装 aarch64-apple-darwin 与 x86_64-apple-darwin 两个 target
# - 已执行 npm --prefix apps/desktop ci
# 脚本自己不安装任何东西。
#
# Tauri 2.11 实测结论（2026-10-14，本机）：
# - 合并方式：macOS 上自动合并 tauri.macos.conf.json，与 --config tauri.bundle.conf.json 同时生效；
#   targets 数组整体替换（无 nsis），createUpdaterArtifacts=false 覆盖主配置的 true（无需签名私钥）。
# - externalBin 后缀：universal 构建要分别编译两个架构，tauri-build 会检查
#   binaries/codex-helper-credential-aarch64-apple-darwin 与 …-x86_64-apple-darwin，
#   打包阶段再读取 …-universal-apple-darwin；三个名字都要有，这里都放同一个 lipo 合成的 fat 文件。
# - dmg 路径：target/universal-apple-darwin/release/bundle/dmg/CodexHelper_<版本>_universal.dmg。
# - dmg 窗口布局要用 AppleScript 驱动 Finder，没有自动化授权的终端或 CI 上会超时失败；
#   设置 CI=true 后 Tauri 传 --skip-jenkins 跳过这一步（dmg 只是没有图标排布），所以默认开启。
# - Tauri CLI 用 rustup 的默认工具链判断 target 是否已安装；本机默认工具链是 Rosetta 下的
#   x86_64 版，因此同时设置 RUSTUP_TOOLCHAIN，指向下面的原生工具链。
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此脚本只能在 macOS 上运行" >&2
  exit 1
fi

# 与 check-windows.sh 相同：优先用 rustup 原生 aarch64 工具链，避开只带 aarch64 标准库的 Homebrew rustc。
TC="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"
if [[ -d "$TC" ]]; then
  export PATH="$TC:$PATH" RUSTC="$TC/rustc" RUSTUP_TOOLCHAIN=stable-aarch64-apple-darwin
fi
export CI="${CI:-true}"

# 预检：缺什么就提示怎么补，然后退出；不在这里安装。
for tool in cargo rustc lipo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "未找到 ${tool}：请先安装 rustup 工具链 stable-aarch64-apple-darwin 与 Xcode 命令行工具" >&2
    exit 1
  fi
done
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
RUSTLIB="$(rustc --print sysroot)/lib/rustlib"
for t in "${TARGETS[@]}"; do
  if [[ ! -d "$RUSTLIB/$t" ]]; then
    echo "当前 rustc 缺少 $t 标准库，请先执行：rustup target add --toolchain stable-aarch64-apple-darwin $t" >&2
    exit 1
  fi
done
if [[ ! -x apps/desktop/node_modules/.bin/tauri ]]; then
  echo "未找到 Tauri CLI，请先执行：npm --prefix apps/desktop ci" >&2
  exit 1
fi

# 校验文件同时含 arm64 与 x86_64。本机 lipo 的 -verify_arch 后面只接受一个架构
# （多个架构会被当成输入文件），所以逐个检查。
verify_universal() {
  local arch
  for arch in arm64 x86_64; do
    if ! lipo "$1" -verify_arch "$arch"; then
      echo "$1 缺少 $arch 架构" >&2
      exit 1
    fi
  done
}

# 凭据程序：两个架构分别编译，lipo 合成 fat 文件，按 Tauri 需要的三个后缀放置（见头部注释）。
SIDECAR=codex-helper-credential
BIN_DIR=apps/desktop/src-tauri/binaries
for t in "${TARGETS[@]}"; do
  cargo build --release -p "$SIDECAR" --target "$t"
done
mkdir -p "$BIN_DIR"
UNIVERSAL="$BIN_DIR/$SIDECAR-universal-apple-darwin"
lipo -create -output "$UNIVERSAL" \
  "target/aarch64-apple-darwin/release/$SIDECAR" "target/x86_64-apple-darwin/release/$SIDECAR"
verify_universal "$UNIVERSAL"
for t in "${TARGETS[@]}"; do
  cp "$UNIVERSAL" "$BIN_DIR/$SIDECAR-$t"
done

# 打包：先删旧产物，避免旧版本 dmg / 升级产物被当成本次结果。
BUNDLE=target/universal-apple-darwin/release/bundle
rm -rf "$BUNDLE/dmg" "$BUNDLE/macos"
# 未提供签名私钥时，用第三个 --config 内联覆盖 createUpdaterArtifacts=false，
# 走降级路径（CLI 按顺序合并 --config，后传覆盖先传，标量覆盖）。
UPDATER_ARGS=()
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "本地构建：未提供签名私钥，跳过升级产物"
  UPDATER_ARGS+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi
# bash 3.2（macOS 自带）在 set -u 下展开空数组会报 unbound variable，
# 用 ${arr[@]+…} 惯用法兼容空数组。
(cd apps/desktop && npx tauri build --target universal-apple-darwin --config src-tauri/tauri.bundle.conf.json ${UPDATER_ARGS[@]+"${UPDATER_ARGS[@]}"})

# 自检：恰好一个 dmg；.app 内两个可执行文件都含两种架构；签名完整。
shopt -s nullglob
dmgs=("$BUNDLE"/dmg/*.dmg)
shopt -u nullglob
if [[ ${#dmgs[@]} -ne 1 ]]; then
  echo "期望恰好 1 个 dmg，实际 ${#dmgs[@]} 个：${dmgs[*]:-无}" >&2
  exit 1
fi
APP="$BUNDLE/macos/CodexHelper.app"
for exe in codex-helper "$SIDECAR"; do
  verify_universal "$APP/Contents/MacOS/$exe"
done
codesign --verify --deep --strict "$APP"

# 升级产物自检：有私钥时恰好一个 .app.tar.gz 且对应 .sig 非空；
# 无私钥时断言 tar.gz 不存在（防旧产物残留误传）。
shopt -s nullglob
targzs=("$BUNDLE"/macos/*.app.tar.gz)
shopt -u nullglob
UPDATER_ARTIFACT=""
if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  if [[ ${#targzs[@]} -ne 1 ]]; then
    echo "期望恰好 1 个 .app.tar.gz 升级产物，实际 ${#targzs[@]} 个：${targzs[*]:-无}" >&2
    exit 1
  fi
  UPDATER_ARTIFACT="${targzs[0]}"
  SIG="$UPDATER_ARTIFACT.sig"
  if [[ ! -s "$SIG" ]]; then
    echo "升级产物签名缺失或为空：$SIG" >&2
    exit 1
  fi
else
  if [[ ${#targzs[@]} -ne 0 ]]; then
    echo "未提供签名私钥却存在 .app.tar.gz（应为降级路径，无升级产物）：${targzs[*]}" >&2
    exit 1
  fi
fi

DMG="$(cd "$(dirname "${dmgs[0]}")" && pwd)/$(basename "${dmgs[0]}")"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "dmg=$DMG" >> "$GITHUB_OUTPUT"
  if [[ -n "$UPDATER_ARTIFACT" ]]; then
    echo "updater_artifact=$(cd "$(dirname "$UPDATER_ARTIFACT")" && pwd)/$(basename "$UPDATER_ARTIFACT")" >> "$GITHUB_OUTPUT"
  fi
fi
echo "产物：$DMG"
if [[ -n "$UPDATER_ARTIFACT" ]]; then
  echo "升级产物：$UPDATER_ARTIFACT（+$UPDATER_ARTIFACT.sig）"
fi
