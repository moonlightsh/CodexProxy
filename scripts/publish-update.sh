#!/usr/bin/env bash
# 生成 Tauri updater 的 latest.json，并（在提供凭据时）上传到内网 Nexus cypub 仓库。
# 详见设计文档 §15 与 2026-10-18-macos-update-design.md（M6：目录扫描式多平台合并）。
# 前置：Windows 端已用 createUpdaterArtifacts=true 且提供 TAURI_SIGNING_PRIVATE_KEY 完成
# `tauri build`（产物 *-setup.exe 与 .sig）；macOS 端由 build-macos.sh 产出
# *.app.tar.gz 与 .sig。发布者从 GitHub Release 下载两平台产物放入同一目录后执行。
#
# 环境变量：
#   NEXUS_DEPLOY_USER / NEXUS_DEPLOY_PASS   上传凭据（两者齐备才上传；否则只生成 latest.json）
#   UPDATE_NOTES                            可选，更新说明（默认按版本号生成一句）
#   BUNDLE_DIR                              可选，覆盖产物目录（默认 target/release/bundle/nsis）
#
# 用法：
#   bash scripts/publish-update.sh              正常发布流程（扫描 + 生成 + 可选上传）
#   bash scripts/publish-update.sh --self-test  自测：不碰网络、不动 BUNDLE_DIR，用临时目录验证
#
# 固定约定（写死，符合本项目“参数写死”思想）：
#   - 下载基址用明文 HTTP（客户端下载用，规避 Nexus 自签证书；完整性由 Ed25519 签名保证）
#   - 上传基址用 HTTPS + curl -k（加密保护部署凭据，跳过自签证书校验）
#   - darwin 两键（darwin-aarch64 / darwin-x86_64）同 URL 同 signature（universal 产物一份）
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DOWNLOAD_BASE="http://rdc.tiandy.com/nexus/repository/cypub/codex-helper"
UPLOAD_BASE="https://rdc.tiandy.com/nexus/repository/cypub/codex-helper"
BUNDLE_DIR="${BUNDLE_DIR:-target/release/bundle/nsis}"

# 从产物文件名解析版本号：CodexHelper_<版本>_xxx → <版本>；无法解析时输出空串。
version_from_artifact_name() {
  local base="$1"
  case "$base" in
    CodexHelper_*_*)
      local rest="${base#CodexHelper_}"
      printf '%s' "${rest%%_*}"
      ;;
    *)
      printf ''
      ;;
  esac
}

# ---------- 生成函数：扫描产物目录，合并生成多平台 latest.json ----------
# 用法：generate_manifest <bundle_dir> <out_json>
# 扫描 *-setup.exe.sig（→ windows-x86_64）与 *.app.tar.gz.sig（→ darwin 两键，各取第一个匹配），
# 配对产物与签名并校验版本一致后生成 manifest；两类产物并存时合并进同一 platforms。
# 一个平台都识别不到时报错退出（空目录不得生成空 platforms 的 manifest）。
generate_manifest() {
  local bundle_dir="$1" out_json="$2"
  local win_sig="" app_sig=""

  shopt -s nullglob
  local candidates=("$bundle_dir"/*-setup.exe.sig)
  if [ ${#candidates[@]} -gt 0 ]; then
    win_sig="${candidates[0]}"
  fi
  candidates=("$bundle_dir"/*.app.tar.gz.sig)
  if [ ${#candidates[@]} -gt 0 ]; then
    app_sig="${candidates[0]}"
  fi
  shopt -u nullglob

  if [ -z "$win_sig" ] && [ -z "$app_sig" ]; then
    echo "错误：$bundle_dir 下未找到任何升级产物签名（*-setup.exe.sig / *.app.tar.gz.sig）。" >&2
    echo "请确认已用 createUpdaterArtifacts=true 完成 tauri build / build-macos.sh，且构建时提供了 TAURI_SIGNING_PRIVATE_KEY。" >&2
    return 1
  fi

  # 配对产物与签名，并校验版本一致性：Windows 与 darwin 产物并存时必须同版本才合并
  # （版本号从产物文件名解析，防止发布目录混入异版本产物）。
  local sigs=()
  if [ -n "$win_sig" ]; then
    sigs+=("$win_sig")
  fi
  if [ -n "$app_sig" ]; then
    sigs+=("$app_sig")
  fi
  local sig art base art_version version=""
  for sig in "${sigs[@]}"; do
    art="${sig%.sig}"
    if [ ! -f "$art" ]; then
      echo "错误：找到签名 ${sig}，但缺少对应产物 ${art}。" >&2
      return 1
    fi
    base="$(basename "$art")"
    art_version="$(version_from_artifact_name "$base")"
    if [ -z "$art_version" ]; then
      echo "错误：无法从产物文件名解析版本号：${base}（期望 CodexHelper_<版本>_ 前缀）。" >&2
      return 1
    fi
    if [ -n "$version" ] && [ "$art_version" != "$version" ]; then
      echo "错误：产物版本不一致：${version} 与 ${art_version}（${base}），拒绝合并异版本产物。" >&2
      return 1
    fi
    version="$art_version"
  done

  local win_name="" app_name=""
  if [ -n "$win_sig" ]; then
    win_name="$(basename "${win_sig%.sig}")"
  fi
  if [ -n "$app_sig" ]; then
    app_name="$(basename "${app_sig%.sig}")"
  fi
  local notes pub_date
  notes="${UPDATE_NOTES:-CodexHelper ${version} 更新。}"
  pub_date="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  # 逐个读取签名文件；读取失败（权限/不存在）直接报错退出，绝不生成空 signature 的 manifest。
  local win_sig_content="" app_sig_content=""
  if [ -n "$win_sig" ]; then
    if ! win_sig_content="$(cat "$win_sig")"; then
      echo "错误：无法读取 Windows 签名文件 $win_sig" >&2
      return 1
    fi
  fi
  if [ -n "$app_sig" ]; then
    if ! app_sig_content="$(cat "$app_sig")"; then
      echo "错误：无法读取 darwin 签名文件 $app_sig" >&2
      return 1
    fi
  fi

  # 用 node 生成 JSON，安全转义 signature/notes；darwin 两键复用同一 url/signature（M2）。
  WIN_SIG="$win_sig_content" \
  APP_SIG="$app_sig_content" \
  WIN_NAME="$win_name" \
  APP_NAME="$app_name" \
  VER="$version" NOTES="$notes" PUB_DATE="$pub_date" BASE_URL="$DOWNLOAD_BASE" \
  node -e '
const fs = require("fs");
const platforms = {};
if (process.env.WIN_NAME) {
  platforms["windows-x86_64"] = {
    signature: process.env.WIN_SIG,
    url: `${process.env.BASE_URL}/${process.env.WIN_NAME}`,
  };
}
if (process.env.APP_NAME) {
  platforms["darwin-aarch64"] = {
    signature: process.env.APP_SIG,
    url: `${process.env.BASE_URL}/${process.env.APP_NAME}`,
  };
  platforms["darwin-x86_64"] = {
    signature: process.env.APP_SIG,
    url: `${process.env.BASE_URL}/${process.env.APP_NAME}`,
  };
}
const manifest = {
  version: process.env.VER,
  notes: process.env.NOTES,
  pub_date: process.env.PUB_DATE,
  platforms,
};
fs.writeFileSync(process.argv[1], JSON.stringify(manifest, null, 2) + "\n");
console.log("平台：" + Object.keys(platforms).join(", "));
' "$out_json"
  echo "已生成 ${out_json} （version=${version}）"
}

# ---------- 自测：不碰网络、不动 BUNDLE_DIR，在临时目录验证生成函数 ----------
# 覆盖：混合目录三平台键、darwin 两键同 URL 同 signature、只含 Windows 时单键（回归）、
# 空目录报错退出码非 0。
self_test() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  local fail=0

  # 场景 1：混合目录（Windows + darwin）→ 三平台键、darwin 两键一致
  mkdir "$tmp/mixed"
  printf 'WIN_EXE' > "$tmp/mixed/CodexHelper_0.4.0_x64-setup.exe"
  printf 'win-signature' > "$tmp/mixed/CodexHelper_0.4.0_x64-setup.exe.sig"
  printf 'APP_TGZ' > "$tmp/mixed/CodexHelper_0.4.0_universal.app.tar.gz"
  printf 'app-signature' > "$tmp/mixed/CodexHelper_0.4.0_universal.app.tar.gz.sig"
  generate_manifest "$tmp/mixed" "$tmp/mixed/latest.json"
  node -e '
const m = JSON.parse(require("fs").readFileSync(process.argv[1]));
const keys = Object.keys(m.platforms);
const want = ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"];
if (keys.length !== 3 || !want.every((k) => keys.includes(k))) {
  console.error("FAIL：混合目录应有三平台键，实际 " + keys.join(","));
  process.exit(1);
}
if (m.platforms["darwin-aarch64"].url !== m.platforms["darwin-x86_64"].url ||
    m.platforms["darwin-aarch64"].signature !== m.platforms["darwin-x86_64"].signature) {
  console.error("FAIL：darwin 两键 url/signature 应相同");
  process.exit(1);
}
if (!m.platforms["windows-x86_64"].url.endsWith("CodexHelper_0.4.0_x64-setup.exe")) {
  console.error("FAIL：windows 键 url 错误 " + m.platforms["windows-x86_64"].url);
  process.exit(1);
}
if (m.version !== "0.4.0") {
  console.error("FAIL：版本号应为 0.4.0，实际 " + m.version);
  process.exit(1);
}
console.log("场景 1 通过：混合目录三平台键、darwin 两键同 URL 同 signature");
' "$tmp/mixed/latest.json" || fail=1

  # 场景 2：只含 Windows 产物 → 单键（Windows 回归不变）
  mkdir "$tmp/winonly"
  printf 'WIN_EXE' > "$tmp/winonly/CodexHelper_0.4.0_x64-setup.exe"
  printf 'win-signature' > "$tmp/winonly/CodexHelper_0.4.0_x64-setup.exe.sig"
  generate_manifest "$tmp/winonly" "$tmp/winonly/latest.json"
  node -e '
const m = JSON.parse(require("fs").readFileSync(process.argv[1]));
const keys = Object.keys(m.platforms);
if (keys.length !== 1 || keys[0] !== "windows-x86_64") {
  console.error("FAIL：仅 Windows 目录应为单键 windows-x86_64，实际 " + keys.join(","));
  process.exit(1);
}
console.log("场景 2 通过：仅 Windows 目录单键 manifest（回归不变）");
' "$tmp/winonly/latest.json" || fail=1

  # 场景 3：空目录 → 报错且退出码非 0
  mkdir "$tmp/empty"
  if generate_manifest "$tmp/empty" "$tmp/empty/latest.json" 2>"$tmp/empty.err"; then
    echo "FAIL：空目录应报错退出，实际成功" >&2
    fail=1
  elif grep -q "未找到任何升级产物签名" "$tmp/empty.err"; then
    echo "场景 3 通过：空目录报错退出（退出码非 0）"
  else
    echo "FAIL：空目录报错信息不符：$(cat "$tmp/empty.err")" >&2
    fail=1
  fi

  # 场景 4：幂等——混合目录重复生成得到相同 manifest（pub_date 时间戳除外）
  generate_manifest "$tmp/mixed" "$tmp/mixed/latest2.json"
  node -e '
const fs = require("fs");
const a = JSON.parse(fs.readFileSync(process.argv[1]));
const b = JSON.parse(fs.readFileSync(process.argv[2]));
delete a.pub_date;
delete b.pub_date;
if (JSON.stringify(a) !== JSON.stringify(b)) {
  console.error("FAIL：重复执行应得到相同 manifest（除 pub_date）");
  process.exit(1);
}
console.log("场景 4 通过：混合目录重复生成 manifest 幂等");
' "$tmp/mixed/latest.json" "$tmp/mixed/latest2.json" || fail=1

  if [ "$fail" -ne 0 ]; then
    echo "self-test：存在失败项。" >&2
    return 1
  fi
  echo "self-test：全部通过。"
}

# ---------- 入口：--self-test 时只跑自测，不进入发布流程 ----------
if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit $?
fi

LATEST_JSON="$BUNDLE_DIR/latest.json"
generate_manifest "$BUNDLE_DIR" "$LATEST_JSON"

# 上传（凭据齐备才执行）。-f：HTTP 错误码即失败退出；-k：跳过自签证书但走 TLS 加密。
# 逐产物 PUT（仅上传实际存在的产物），最后覆盖上传合并后的 latest.json。
if [ -n "${NEXUS_DEPLOY_USER:-}" ] && [ -n "${NEXUS_DEPLOY_PASS:-}" ]; then
  echo "上传产物与 manifest 到 Nexus（覆盖式 redeploy）……"
  shopt -s nullglob
  for sig in "$BUNDLE_DIR"/*-setup.exe.sig "$BUNDLE_DIR"/*.app.tar.gz.sig; do
    art="${sig%.sig}"
    art_name="$(basename "$art")"
    curl -fk -u "$NEXUS_DEPLOY_USER:$NEXUS_DEPLOY_PASS" --upload-file "$art" "$UPLOAD_BASE/$art_name"
  done
  shopt -u nullglob
  curl -fk -u "$NEXUS_DEPLOY_USER:$NEXUS_DEPLOY_PASS" --upload-file "$LATEST_JSON" "$UPLOAD_BASE/latest.json"
  echo "上传完成。检测地址：$DOWNLOAD_BASE/latest.json"
else
  echo "未提供 NEXUS_DEPLOY_USER/NEXUS_DEPLOY_PASS，跳过上传（仅生成 latest.json）。"
  echo "可在能访问 rdc.tiandy.com 的机器上设置这两个环境变量后重跑本脚本完成发布。"
fi
