// 受管模式开关（role=switch，设计 §8）。
import { el } from "./dom";
import type { Status } from "./types";

export interface SwitchHandlers {
  /** 打开开关：调用方负责必要时先弹出 catalog 确认，再调用 enable。 */
  onTurnOn: () => void;
  onTurnOff: () => void;
}

/** 区分进行中的是启用还是停用，以便展示不同的进行中文案（任务 3.2 要求 6）。 */
export interface SwitchBusyState {
  enabling: boolean;
  disabling: boolean;
}

export interface SwitchRegion {
  root: HTMLElement;
  update: (status: Status, busyState: SwitchBusyState) => void;
}

export function createSwitchRegion(handlers: SwitchHandlers): SwitchRegion {
  const button = el("button", {
    type: "button",
    class: "switch-control",
    role: "switch",
    "aria-checked": "false",
    "aria-label": "受管模式",
  });
  const label = el("span", { class: "switch-label" }, ["受管模式"]);
  const stateText = el("span", { class: "switch-state-text" }, ["已停用"]);
  const root = el("div", { class: "switch-row" }, [label, button, stateText]);

  let currentEnabled = false;

  button.addEventListener("click", () => {
    if (button.disabled) return;
    if (currentEnabled) {
      handlers.onTurnOff();
    } else {
      handlers.onTurnOn();
    }
  });

  const update = (status: Status, busyState: SwitchBusyState) => {
    currentEnabled = status.enabled;
    button.setAttribute("aria-checked", String(status.enabled));
    button.classList.toggle("switch-on", status.enabled);
    const busy = busyState.enabling || busyState.disabling;
    stateText.textContent = busyState.enabling
      ? "启用中…"
      : busyState.disabling
        ? "停用中…"
        : status.enabled
          ? "已启用"
          : "已停用";
    if (busy) {
      button.setAttribute("aria-busy", "true");
    } else {
      button.removeAttribute("aria-busy");
    }
    const canToggle = status.enabled || status.keyConfigured;
    button.disabled = busy || !canToggle;
  };

  return { root, update };
}
