// 开关（role=switch）。受管模式与“启用本地代理”共用 createToggle。
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

export interface ToggleView {
  enabled: boolean;
  busy: boolean;
  label: string;
  ariaLabel: string;
  onText: string;
  offText: string;
  busyText: string;
}

export interface ToggleControl {
  root: HTMLElement;
  update: (view: ToggleView) => void;
}

/** 通用开关。忙时禁用并显示 busyText；点击回调的是切换后的目标状态。 */
export function createToggle(handlers: { onChange: (enabled: boolean) => void }): ToggleControl {
  const button = el("button", {
    type: "button",
    class: "switch-control",
    role: "switch",
    "aria-checked": "false",
  });
  const label = el("span", { class: "switch-label" });
  const stateText = el("span", { class: "switch-state-text" });
  const root = el("div", { class: "switch-row" }, [label, button, stateText]);

  let currentEnabled = false;

  button.addEventListener("click", () => {
    if (button.disabled) return;
    handlers.onChange(!currentEnabled);
  });

  const update = (view: ToggleView) => {
    currentEnabled = view.enabled;
    label.textContent = view.label;
    button.setAttribute("aria-label", view.ariaLabel);
    button.setAttribute("aria-checked", String(view.enabled));
    button.classList.toggle("switch-on", view.enabled);
    stateText.textContent = view.busy ? view.busyText : view.enabled ? view.onText : view.offText;
    if (view.busy) {
      button.setAttribute("aria-busy", "true");
    } else {
      button.removeAttribute("aria-busy");
    }
    button.disabled = view.busy;
  };

  return { root, update };
}

export function createSwitchRegion(handlers: SwitchHandlers): SwitchRegion {
  const toggle = createToggle({
    onChange: (enabled) => {
      if (enabled) handlers.onTurnOn();
      else handlers.onTurnOff();
    },
  });

  const update = (status: Status, busyState: SwitchBusyState) => {
    const busy = busyState.enabling || busyState.disabling;
    toggle.update({
      enabled: status.enabled,
      busy,
      label: "受管模式",
      ariaLabel: "受管模式",
      onText: "已启用",
      offText: "已停用",
      busyText: busyState.enabling ? "启用中…" : "停用中…",
    });
    // 未配置 Key 且未启用时不能打开受管开关；createToggle 只按 busy 禁用。
    const button = toggle.root.querySelector("button");
    if (button instanceof HTMLButtonElement) {
      button.disabled = busy || !(status.enabled || status.keyConfigured);
    }
  };

  return { root: toggle.root, update };
}
