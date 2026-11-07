// 组件版本区（组件更新检查设计 §4.4）：手动检查 ChatGPT Desktop 与 Codex CLI 的本地/远端版本。
// 检测和代理下载在 Rust 侧执行，本模块负责展示和回调，不安装。
// 下载地址只以可选中文本展示（不做链接、不自动打开浏览器——内网机器打开也没用）。
import {
  componentCheckButton,
  componentEmptyText,
  componentNetworkText,
  componentRowViewState,
  type ComponentId,
  type ComponentRowView,
} from "./componentformat";
import { formatClock } from "./devenvformat";
import { downloadView, type ComponentDownloadState } from "./componentdownload";
import { clear, el } from "./dom";
import type { ComponentUpdates } from "./types";

export interface ComponentPanelHandlers {
  /** 用户点击“检查组件更新”。 */
  onCheck: () => void;
  onDownload: (component: ComponentId, version: string) => void;
  onCancel: (component: ComponentId) => void;
}

export interface ComponentPanelState {
  checking: boolean;
  result: ComponentUpdates | null;
  /** 最近一次检查完成的时间（Unix 毫秒）；尚未检查为 null */
  checkedAt: number | null;
  downloads: Partial<Record<ComponentId, ComponentDownloadState>>;
  proxyReady: boolean;
}

export interface ComponentPanelRegion {
  root: HTMLElement;
  update: (state: ComponentPanelState) => void;
}

const COMPONENT_ORDER: ComponentId[] = ["chatgptDesktop", "codexCli"];

function rowElement(view: ComponentRowView): HTMLElement {
  return el("div", { class: "component-row" }, [
    el("div", { class: "component-row-main" }, [
      el("span", { class: "component-label" }, [view.label]),
      el("span", { class: "component-versions" }, [`本地 ${view.localText} · 最新 ${view.remoteText}`]),
      el("span", { class: `component-badge component-badge-${view.badge.level}` }, [view.badge.text]),
    ]),
    view.note ? el("p", { class: "component-note" }, [view.note]) : null,
    view.download
      ? el("div", { class: "component-download" }, [
          el("p", { class: "component-download-hint" }, [view.download.hint]),
          el("pre", { class: "component-url" }, [view.download.url]),
        ])
      : null,
  ]);
}

export function createComponentPanel(handlers: ComponentPanelHandlers): ComponentPanelRegion {
  const checkButton = el("button", { type: "button", class: "btn btn-sm" }, [
    componentCheckButton(false).text,
  ]) as HTMLButtonElement;
  const checkedText = el("span", { class: "component-checked" });
  const networkText = el("p", { class: "component-network" });
  const results = el("div", { class: "component-results", "aria-live": "polite" });

  checkButton.addEventListener("click", () => handlers.onCheck());

  const root = el("div", { class: "component-region" }, [
    el("div", { class: "component-header" }, [
      el("h2", { class: "section-title" }, ["组件版本"]),
      checkedText,
      checkButton,
    ]),
    networkText,
    results,
    el("p", { class: "footer-hint" }, [
      "ChatGPT Desktop 自带的 codex.exe 随应用更新，不在 Codex CLI 行统计。",
    ]),
    el("p", { class: "footer-hint" }, [
      "下载需先启用本地代理，文件默认保存到系统“下载”目录下的 CodexHelper 文件夹，完成后请自行安装。",
    ]),
  ]);

  let renderedResult: ComponentUpdates | null | undefined;
  let renderedChecking: boolean | undefined;
  const controls = new Map<ComponentId, {
    button: HTMLButtonElement; cancel: HTMLButtonElement; progress: HTMLProgressElement;
    text: HTMLElement; path: HTMLElement;
  }>();

  const update = (state: ComponentPanelState) => {
    const downloading = Object.values(state.downloads).some((task) => task?.kind === "downloading");
    const button = componentCheckButton(state.checking);
    checkButton.disabled = button.disabled || downloading;
    checkButton.textContent = button.text;
    checkedText.textContent = state.checkedAt !== null ? `上次检查 ${formatClock(state.checkedAt)}` : "";
    checkedText.hidden = state.checkedAt === null;
    results.setAttribute("aria-busy", state.checking ? "true" : "false");

    // status-changed 每 30 秒触发一次整页 render()；结果没变时不重建结果区，
    // 否则用户正在选中复制的下载地址会被清掉。
    const result = state.result;
    if (renderedResult !== result || (result === null && renderedChecking !== state.checking)) {
      renderedResult = result;
      renderedChecking = state.checking;
      clear(results);
      controls.clear();
      networkText.textContent = result ? componentNetworkText(result) : "";
      networkText.hidden = result === null;
      if (!result) {
        results.append(el("p", { class: "component-empty" }, [componentEmptyText(state.checking)]));
        return;
      }
      for (const id of COMPONENT_ORDER) {
        const view = componentRowViewState(id, result[id]);
        const row = rowElement(view);
        const button = el("button", { type: "button", class: "btn btn-sm" }, ["通过代理下载"]) as HTMLButtonElement;
        button.hidden = view.download === null;
        const remote = result[id].remote;
        button.addEventListener("click", () => { if (remote.kind === "ok") handlers.onDownload(id, remote.version); });
        const cancel = el("button", { type: "button", class: "btn btn-sm" }, ["取消下载"]) as HTMLButtonElement;
        cancel.addEventListener("click", () => handlers.onCancel(id));
        const progress = el("progress", { class: "component-progress", max: "100", "aria-label": `${view.label} 下载进度` }) as HTMLProgressElement;
        const text = el("p", { class: "component-download-state", "aria-live": "polite" });
        const path = el("pre", { class: "component-url" });
        row.append(el("div", { class: "component-download-actions" }, [button, cancel]), progress, text, path);
        results.append(row);
        controls.set(id, { button, cancel, progress, text, path });
      }
    }
    for (const [id, control] of controls) {
      const task = state.downloads[id];
      const view = downloadView(task);
      control.button.disabled = downloading || state.checking || !state.proxyReady;
      control.button.title = state.proxyReady ? "" : "请先启用本地代理";
      control.button.textContent = task?.kind === "completed" ? "再次下载" : task?.kind === "failed" || task?.kind === "cancelled" ? "重试下载" : "通过代理下载";
      control.cancel.hidden = task?.kind !== "downloading";
      control.cancel.disabled = !view.canCancel;
      control.progress.hidden = task?.kind !== "downloading";
      if (view.percent === null) control.progress.removeAttribute("value");
      else control.progress.value = view.percent;
      control.text.textContent = view.text;
      control.text.hidden = view.text === "";
      control.path.textContent = view.path ?? "";
      control.path.hidden = view.path === null;
    }
  };

  return { root, update };
}
