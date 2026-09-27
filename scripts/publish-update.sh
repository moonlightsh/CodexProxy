#!/usr/bin/env bash
# 生成 Tauri updater 的 latest.json，并（在提供凭据时）上传到内网 Nexus cypub 仓库。
# 详见设计文档 §15。前置：已用 createUpdaterArtifacts=true 且提供 TAURI_SIGNING_PRIVATE_KEY
# 完成 `tauri build`，产物 *-setup.exe 与 *-setup.exe.sig 位于 BUNDLE_DIR。
#
# 环境变量：
#   NEXUS_DEPLOY_USER / NEXUS_DEPLOY_PASS   上传凭据（两者齐备才上传；否则只生成 latest.json）
#   UPDATE_NOTES                            可选，更新说明（默认按版本号生成一句）
#   BUNDLE_DIR                              可选，覆盖产物目录（默认 target/release/bundle/nsis）
#
# 固定约定（写死，符合本项目“参数写死”思想）：
#   - 下载基址用明文 HTTP（客户端下载用，规避 Nexus 自签证书；完整性由 Ed25519 签名保证）
#   - 上传基址用 HTTPS + curl -k（加密保护部署凭据，跳过自签证书校验）
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DOWNLOAD_BASE="http://rdc.tiandy.com/nexus/repository/cypub/codex-helper"
UPLOAD_BASE="https://rdc.tiandy.com/nexus/repository/cypub/codex-helper"
BUNDLE_DIR="${BUNDLE_DIR:-target/release/bundle/nsis}"

VERSION="$(node -p "require('./apps/desktop/src-tauri/tauri.conf.json').version")"

# 定位签名文件；被签名的产物 = 去掉 .sig 后缀（createUpdaterArtifacts=true 下即 *-setup.exe）。
shopt -s nullglob
sig_files=("$BUNDLE_DIR"/*.sig)
shopt -u nullglob
if [ ${#sig_files[@]} -eq 0 ]; then
  echo "错误：$BUNDLE_DIR 下未找到 .sig 签名文件。" >&2
  echo "请确认已用 createUpdaterArtifacts=true 完成 tauri build，且构建时提供了 TAURI_SIGNING_PRIVATE_KEY。" >&2
  exit 1
fi
SIG_FILE="${sig_files[0]}"
ARTIFACT="${SIG_FILE%.sig}"
if [ ! -f "$ARTIFACT" ]; then
  echo "错误：找到签名 ${SIG_FILE}，但缺少对应产物 ${ARTIFACT}。" >&2
  exit 1
fi
ARTIFACT_NAME="$(basename "$ARTIFACT")"
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
NOTES="${UPDATE_NOTES:-CodexHelper ${VERSION} 更新。}"
LATEST_JSON="$BUNDLE_DIR/latest.json"

# 用 node 生成 JSON，安全转义 signature/notes。
SIG_CONTENT="$(cat "$SIG_FILE")" \
ARTIFACT_URL="$DOWNLOAD_BASE/$ARTIFACT_NAME" \
VER="$VERSION" NOTES="$NOTES" PUB_DATE="$PUB_DATE" \
node -e '
const fs = require("fs");
const manifest = {
  version: process.env.VER,
  notes: process.env.NOTES,
  pub_date: process.env.PUB_DATE,
  platforms: {
    "windows-x86_64": {
      signature: process.env.SIG_CONTENT,
      url: process.env.ARTIFACT_URL,
    },
  },
};
fs.writeFileSync(process.argv[1], JSON.stringify(manifest, null, 2) + "\n");
' "$LATEST_JSON"
echo "已生成 $LATEST_JSON （version=$VERSION, url=$DOWNLOAD_BASE/${ARTIFACT_NAME}）"

# 上传（凭据齐备才执行）。-f：HTTP 错误码即失败退出；-k：跳过自签证书但仍走 TLS 加密。
if [ -n "${NEXUS_DEPLOY_USER:-}" ] && [ -n "${NEXUS_DEPLOY_PASS:-}" ]; then
  echo "上传产物与 manifest 到 Nexus（覆盖式 redeploy）……"
  curl -fk -u "$NEXUS_DEPLOY_USER:$NEXUS_DEPLOY_PASS" --upload-file "$ARTIFACT"    "$UPLOAD_BASE/$ARTIFACT_NAME"
  curl -fk -u "$NEXUS_DEPLOY_USER:$NEXUS_DEPLOY_PASS" --upload-file "$LATEST_JSON" "$UPLOAD_BASE/latest.json"
  echo "上传完成。检测地址：$DOWNLOAD_BASE/latest.json"
else
  echo "未提供 NEXUS_DEPLOY_USER/NEXUS_DEPLOY_PASS，跳过上传（仅生成 latest.json）。"
  echo "可在能访问 rdc.tiandy.com 的机器上设置这两个环境变量后重跑本脚本完成发布。"
fi
