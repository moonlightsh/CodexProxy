// 组件版本区（组件更新检查设计 §4.4）：手动检查 ChatGPT Desktop 与 Codex CLI 的本地/远端版本。
// 只检测、不下载不安装；检测逻辑在 Rust 侧命令（api.checkComponentUpdates），本模块只管展示与回调。
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
import { clear, el } from "./dom";
import type { ComponentUpdates } from "./types";

export interface ComponentPanelHandlers {
  /** 用户点击“检查组件更新”。 */
  onCheck: () => void;
}

export interface ComponentPanelState {
  checking: boolean;
  result: ComponentUpdates | null;
  /** 最近一次检查完成的时间（Unix 毫秒）；尚未检查为 null */
  checkedAt: number | null;
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
  ]);

  let rendered: ComponentPanelState | null = null;

  const update = (state: ComponentPanelState) => {
    const button = componentCheckButton(state.checking);
    checkButton.disabled = button.disabled;
    checkButton.textContent = button.text;
    checkedText.textContent = state.checkedAt !== null ? `上次检查 ${formatClock(state.checkedAt)}` : "";
    checkedText.hidden = state.checkedAt === null;
    results.setAttribute("aria-busy", state.checking ? "true" : "false");

    // status-changed 每 30 秒触发一次整页 render()；结果没变时不重建结果区，
    // 否则用户正在选中复制的下载地址会被清掉。
    if (
      rendered !== null &&
      rendered.result === state.result &&
      rendered.checking === state.checking &&
      rendered.checkedAt === state.checkedAt
    ) {
      return;
    }
    rendered = { result: state.result, checking: state.checking, checkedAt: state.checkedAt };

    clear(results);
    const result = state.result;
    networkText.textContent = result ? componentNetworkText(result) : "";
    networkText.hidden = result === null;
    if (!result) {
      results.append(el("p", { class: "component-empty" }, [componentEmptyText(state.checking)]));
      return;
    }
    for (const id of COMPONENT_ORDER) {
      results.append(rowElement(componentRowViewState(id, result[id])));
    }
  };

  return { root, update };
}
