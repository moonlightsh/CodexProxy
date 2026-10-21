// 组件更新检查区块的纯函数（组件更新检查设计 §4.4）：检测结果 → 行视图文案与徽标。不依赖 DOM。
// 失败原因（detail / directReason）由 Rust 侧给出固定文案，这里只负责拼装展示，不自行推断。
import type { StatusLevel } from "./format";
import type { ComponentUpdate, ComponentUpdates, LocalState, RemoteState } from "./types";

export type ComponentId = "chatgptDesktop" | "codexCli";

export const COMPONENT_LABELS: Record<ComponentId, string> = {
  chatgptDesktop: "ChatGPT Desktop",
  // ChatGPT Desktop 自带的 codex.exe 随应用更新，不在本行统计
  codexCli: "Codex CLI（独立安装）",
};

export interface ComponentRowView {
  label: string;
  localText: string;
  remoteText: string;
  badge: { text: string; level: StatusLevel };
  /** 缓存兜底说明等补充信息；无则 null */
  note: string | null;
  /** 仅 comparison === "updateAvailable" 且 downloadUrl 非空时有值 */
  download: { hint: string; url: string } | null;
}

const UNSUPPORTED_TEXT = "暂不支持（二期）";

const DOWNLOAD_HINTS: Record<ComponentId, string> = {
  chatgptDesktop: "请通过离线包管线更新（离线安装还需 ChatGPT-License.xml）。官方安装包地址：",
  codexCli: "请通过离线包管线更新 Codex CLI。发布页：",
};

export function componentRowViewState(id: ComponentId, row: ComponentUpdate): ComponentRowView {
  return {
    label: COMPONENT_LABELS[id],
    localText: localText(row.local),
    remoteText: remoteText(row.remote),
    badge: badge(id, row),
    note: cacheNote(row.remote),
    download:
      row.comparison === "updateAvailable" && row.downloadUrl
        ? { hint: DOWNLOAD_HINTS[id], url: row.downloadUrl }
        : null,
  };
}

function localText(local: LocalState): string {
  switch (local.kind) {
    case "installed":
      return local.version;
    case "notInstalled":
      return "未安装";
    case "notRegisteredForUser":
      return "未安装或当前用户未注册（预配安装后可能需重新登录）";
    case "probeFailed":
      return `探测失败：${local.detail}`;
    case "unsupportedPlatform":
      return UNSUPPORTED_TEXT;
  }
}

function remoteText(remote: RemoteState): string {
  switch (remote.kind) {
    case "ok":
      return remote.version;
    case "unreachable":
      return `远端不可达：${remote.detail}`;
    case "invalid":
      return `清单无效：${remote.detail}`;
    case "blocked":
      return remote.detail;
    case "unsupportedPlatform":
      return UNSUPPORTED_TEXT;
  }
}

/** 版本核心段（去掉 -预发布 / +构建 后缀）为 0.0.0 视为开发版（设计 C6）。 */
function isDevBuild(version: string): boolean {
  return version.split(/[-+]/, 1)[0] === "0.0.0";
}

/** 状态徽标：按顺序取第一条命中。 */
function badge(id: ComponentId, row: ComponentUpdate): ComponentRowView["badge"] {
  const { local, remote } = row;
  switch (row.comparison) {
    case "updateAvailable":
      return { text: remote.kind === "ok" ? `有新版本 v${remote.version}` : "有新版本", level: "warn" };
    case "upToDate":
      return { text: "已是最新", level: "ok" };
    case "localNewer":
      return { text: "本地较新", level: "ok" };
    case "unknown":
      break;
  }
  if (remote.kind === "unsupportedPlatform" || local.kind === "unsupportedPlatform") {
    return { text: UNSUPPORTED_TEXT, level: "warn" };
  }
  if (id === "codexCli" && local.kind === "installed" && isDevBuild(local.version)) {
    return { text: "开发版，不比较", level: "warn" };
  }
  if (remote.kind === "blocked") return { text: "Secure 代理未就绪", level: "bad" };
  if (remote.kind === "unreachable") return { text: "远端不可达", level: "bad" };
  if (remote.kind === "invalid") return { text: "清单无效", level: "bad" };
  if (local.kind === "notInstalled") return { text: "未安装", level: "warn" };
  if (local.kind === "notRegisteredForUser") return { text: "未注册", level: "warn" };
  if (local.kind === "probeFailed") return { text: "探测失败", level: "bad" };
  return { text: "未知", level: "warn" };
}

/** 本地缓存兜底时说明远端失败原因与缓存时间（checkedAt 原样显示，不做时区换算）。 */
function cacheNote(remote: RemoteState): string | null {
  if (remote.kind !== "ok" || remote.source.kind !== "codexLocalCache") return null;
  return `远端不可达（${remote.fallbackFrom ?? "原因未知"}），以下为 Codex 本地缓存，检查于 ${remote.source.checkedAt}`;
}

/** 网络路径说明（按顺序判定）。 */
export function componentNetworkText(result: ComponentUpdates): string {
  const rows = [result.chatgptDesktop, result.codexCli];
  if (rows.some((row) => row.remote.kind === "blocked")) return "未发出远端请求（Secure 代理未就绪）";
  if (rows.some((row) => row.viaProxy)) return "经本地代理检测";
  const reason = rows.find((row) => row.directReason !== null)?.directReason;
  return reason ? `直连检测：${reason}` : "直连检测";
}

export function componentCheckButton(checking: boolean): { text: string; disabled: boolean } {
  return checking ? { text: "检查中…", disabled: true } : { text: "检查组件更新", disabled: false };
}

/**
 * 尚无检测结果时结果区的占位文案。
 * 检查中如实提示最坏耗时：本地探测（子进程 15s，macOS 另加登录 shell 5s）后，
 * CLI 远端还可能顺序经历 API 与 302 回退各 15s 超时，合计约 50s。
 */
export function componentEmptyText(checking: boolean): string {
  return checking
    ? "正在检查：先探测本地版本，再请求远端；网络不佳时最长可能需要约 1 分钟…"
    : "点击“检查组件更新”查看 ChatGPT Desktop 与 Codex CLI 的最新版本。";
}
