// 开发环境页（设计 §16）：检测 Python 3.13+ / Node.js，已安装时再检测 pip / npm 镜像源。
// 只检测、不修改；检测逻辑在 Rust 侧命令（api.detectDevEnv），本模块只管展示与回调。
import { clear, el } from "./dom";
import { devEnvRows, formatClock, type DevEnvRow } from "./devenvformat";
import type { DevEnvReport } from "./types";

export interface DevEnvPanelHandlers {
  /** 用户点击“开始检测 / 重新检测”。 */
  onDetect: () => void;
}

export interface DevEnvPanelState {
  detecting: boolean;
  report: DevEnvReport | null;
  /** 最近一次检测完成的时间（Unix 毫秒）；尚未检测为 null */
  checkedAt: number | null;
}

export interface DevEnvPanelRegion {
  root: HTMLElement;
  update: (state: DevEnvPanelState) => void;
}

function rowElement(row: DevEnvRow): HTMLElement {
  const hint = row.hint
    ? el("div", { class: "devenv-hint" }, [
        el("p", { class: "devenv-hint-text" }, [row.hint.text]),
        row.hint.code ? el("pre", { class: "devenv-code" }, [row.hint.code]) : null,
      ])
    : null;
  return el("div", { class: "devenv-row" }, [
    el("div", { class: `status-light status-light-${row.level}` }, [
      el("span", { class: "status-dot", "aria-hidden": "true" }),
      el("span", { class: "status-light-label" }, [row.label]),
      el("span", { class: "status-light-addr devenv-addr" }, [row.addr]),
      el("span", { class: "status-light-value" }, [row.value]),
    ]),
    hint,
  ]);
}

export function createDevEnvPanel(handlers: DevEnvPanelHandlers): DevEnvPanelRegion {
  const detectButton = el("button", { type: "button", class: "btn btn-sm btn-primary" }, [
    "开始检测",
  ]) as HTMLButtonElement;
  const checkedText = el("span", { class: "devenv-checked" });
  const results = el("div", { class: "status-lights devenv-results" });
  const devBanner = el("div", { class: "banner banner-dev" }, [
    "开发模式：非 Windows 平台按 python3 检测，结果仅供调试。",
  ]);

  detectButton.addEventListener("click", () => handlers.onDetect());

  const root = el("div", { class: "devenv-panel" }, [
    el("p", { class: "devenv-intro" }, [
      "检查本机是否安装了 Python 3.13+ 与 Node.js；已安装的，再检查 pip / npm 是否启用了内网可访问的镜像源。本页只检测，不修改任何配置。",
    ]),
    devBanner,
    el("div", { class: "devenv-toolbar" }, [detectButton, checkedText]),
    results,
    el("p", { class: "footer-hint" }, [
      "检测的是命令行里直接输入 python / node / npm 时实际用到的程序。安装或修改 Path 后，已打开的终端与 AI 编程工具需要重启才会生效。",
    ]),
  ]);

  let rendered: DevEnvPanelState | null = null;

  const update = (state: DevEnvPanelState) => {
    detectButton.disabled = state.detecting;
    detectButton.textContent = state.detecting ? "检测中…" : state.report ? "重新检测" : "开始检测";
    checkedText.textContent =
      state.checkedAt !== null ? `上次检测 ${formatClock(state.checkedAt)}` : "尚未检测";
    results.setAttribute("aria-busy", state.detecting ? "true" : "false");
    devBanner.hidden = !state.report || state.report.platformSupported;

    // status-changed 每 30 秒触发一次整页 render()；结果没变时不重建结果区，
    // 否则用户正在选中复制的修复提示会被清掉。
    if (
      rendered !== null &&
      rendered.report === state.report &&
      rendered.detecting === state.detecting &&
      rendered.checkedAt === state.checkedAt
    ) {
      return;
    }
    rendered = { report: state.report, detecting: state.detecting, checkedAt: state.checkedAt };

    clear(results);
    if (!state.report) {
      results.append(
        el("p", { class: "devenv-empty" }, [state.detecting ? "正在检测，通常需要几秒…" : "点击“开始检测”。"]),
      );
      return;
    }
    for (const row of devEnvRows(state.report)) {
      results.append(rowElement(row));
    }
  };

  return { root, update };
}
