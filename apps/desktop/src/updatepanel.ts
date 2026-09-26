// 更新区：显示当前版本、检查更新，发现新版本后由用户确认再安装（设计 §15）。
// 检测/安装逻辑在 Rust 侧命令（api.checkUpdate / api.installUpdate），本模块只管展示与回调。
import { el } from "./dom";
import type { UpdateCheck } from "./types";

export interface UpdatePanelHandlers {
  /** 用户点击“检查更新”。 */
  onCheck: () => void;
  /** 用户点击“立即更新”（发现新版本后可见）。 */
  onInstall: () => void;
}

export interface UpdatePanelState {
  /** 当前运行版本；未知时为 null（尚未成功检测过）。 */
  currentVersion: string | null;
  /** 正在检查更新。 */
  checking: boolean;
  /** 正在下载/安装更新。 */
  installing: boolean;
  /** 最近一次检测到的可用更新；无可用更新为 null。 */
  available: UpdateCheck | null;
}

export interface UpdatePanelRegion {
  root: HTMLElement;
  update: (state: UpdatePanelState) => void;
}

export function createUpdatePanel(handlers: UpdatePanelHandlers): UpdatePanelRegion {
  const versionText = el("span", { class: "update-version" }, ["当前版本 —"]);
  const checkButton = el("button", { type: "button", class: "btn btn-sm" }, [
    "检查更新",
  ]) as HTMLButtonElement;
  const installButton = el("button", { type: "button", class: "btn btn-sm btn-primary" }, [
    "立即更新",
  ]) as HTMLButtonElement;
  const hint = el("p", { class: "update-hint" });

  checkButton.addEventListener("click", () => handlers.onCheck());
  installButton.addEventListener("click", () => handlers.onInstall());

  const root = el("div", { class: "update-region" }, [
    el("div", { class: "update-row" }, [versionText, checkButton, installButton]),
    hint,
  ]);

  const update = (state: UpdatePanelState) => {
    versionText.textContent = state.currentVersion
      ? `当前版本 v${state.currentVersion}`
      : "当前版本 —";

    const busy = state.checking || state.installing;
    checkButton.disabled = busy;
    checkButton.textContent = state.checking ? "检查中…" : "检查更新";

    const available = state.available;
    installButton.hidden = available === null;
    installButton.disabled = state.installing;
    installButton.textContent = installLabel(state);

    if (state.installing) {
      hint.textContent = "正在下载并安装更新，完成后会自动重启应用…";
      hint.hidden = false;
    } else if (available) {
      hint.textContent = available.notes?.trim()
        ? `发现新版本 v${available.version}：${available.notes.trim()}`
        : `发现新版本 v${available.version}，可点击“立即更新”安装。`;
      hint.hidden = false;
    } else {
      hint.textContent = "";
      hint.hidden = true;
    }
  };

  return { root, update };
}

/** “立即更新”按钮文案：安装中优先，其次带上目标版本号。 */
function installLabel(state: UpdatePanelState): string {
  if (state.installing) return "安装中…";
  if (state.available?.version) return `更新到 v${state.available.version}`;
  return "立即更新";
}
