// 网关设置：模型网关、上游 SOCKS5、本地代理开关。格式校验在服务端，本面板只展示结果。
import { clear, el } from "./dom";
import { endpointWarningText, shouldKeepEndpointDraft, socks5Muted } from "./endpointformat";
import { createToggle } from "./switchcontrol";
import type { EndpointWarning, Status } from "./types";

const INTRANET_GATEWAY_URL = "http://10.20.30.61:8080";
const INTERNET_GATEWAY_URL = "https://39.106.80.79/";

export interface EndpointPanelHandlers {
  /** 保存当前输入框内容，不做格式校验。 */
  onSave: (gatewayBaseUrl: string, socks5Addr: string) => void;
  onRestoreDefaults: () => void;
  onProxyChange: (enabled: boolean) => void;
  onReenterKey: () => void;
}

export interface EndpointPanel {
  root: HTMLElement;
  update: (status: Status, busy: ReadonlySet<string>) => void;
  showFieldError: (field: "gateway" | "socks5", message: string) => void;
  showWarnings: (warnings: EndpointWarning[], source: "endpoints" | "proxy") => void;
  showMessage: (message: string) => void;
  /** 清掉 dirty，供保存成功后的下一次 update 写入返回状态。 */
  markClean: () => void;
  clearFeedback: () => void;
}

export function createEndpointPanel(handlers: EndpointPanelHandlers): EndpointPanel {
  const gatewayInput = el("input", {
    type: "text",
    id: "endpoint-gateway",
    autocomplete: "off",
    spellcheck: "false",
    "aria-label": "模型网关",
  }) as HTMLInputElement;
  const gatewayPreset = el("select", {
    class: "endpoint-gateway-preset",
    "aria-label": "模型网关预设",
  }, [
    el("option", { value: "", selected: true, disabled: true, hidden: true }, ["选择预设"]),
    el("option", { value: INTRANET_GATEWAY_URL }, [`${INTRANET_GATEWAY_URL} (内网)`]),
    el("option", { value: INTERNET_GATEWAY_URL }, [`${INTERNET_GATEWAY_URL} (外网)`]),
  ]);
  const socks5Input = el("input", {
    type: "text",
    id: "endpoint-socks5",
    autocomplete: "off",
    spellcheck: "false",
    "aria-label": "上游 SOCKS5",
  }) as HTMLInputElement;
  const gatewayError = el("p", { class: "endpoint-field-error" });
  const socks5Error = el("p", { class: "endpoint-field-error" });
  gatewayError.hidden = true;
  socks5Error.hidden = true;

  const proxyToggle = createToggle({
    onChange: (enabled) => handlers.onProxyChange(enabled),
    hint: { id: "endpoint-proxy-hint", text: "使用外网网关时无需开启本地代理" },
  });

  const restoreButton = el("button", { type: "button", class: "btn" }, ["恢复默认"]) as HTMLButtonElement;
  const saveButton = el("button", { type: "button", class: "btn btn-primary" }, ["保存"]) as HTMLButtonElement;
  const feedback = el("div", { class: "endpoint-feedback", "aria-live": "polite" });

  const root = el("div", { class: "endpoint-panel" }, [
    el("h2", { class: "section-title" }, ["网关设置"]),
    el("div", { class: "endpoint-row endpoint-gateway-row" }, [
      el("label", { for: gatewayInput.id }, ["模型网关"]),
      gatewayInput,
      gatewayPreset,
    ]),
    gatewayError,
    el("div", { class: "endpoint-row" }, [el("span", {}, ["上游 SOCKS5"]), socks5Input]),
    socks5Error,
    proxyToggle.root,
    el("div", { class: "endpoint-actions" }, [restoreButton, saveButton]),
    feedback,
  ]);

  let gatewayFocused = false;
  let socks5Focused = false;
  let gatewayDirty = false;
  let socks5Dirty = false;

  gatewayInput.addEventListener("input", () => {
    gatewayDirty = true;
    gatewayPreset.value = "";
  });
  gatewayPreset.addEventListener("change", () => {
    gatewayInput.value = gatewayPreset.value;
    gatewayDirty = true;
  });
  socks5Input.addEventListener("input", () => {
    socks5Dirty = true;
  });
  gatewayInput.addEventListener("focus", () => {
    gatewayFocused = true;
  });
  socks5Input.addEventListener("focus", () => {
    socks5Focused = true;
  });
  gatewayInput.addEventListener("blur", () => {
    gatewayFocused = false;
  });
  socks5Input.addEventListener("blur", () => {
    socks5Focused = false;
  });

  saveButton.addEventListener("click", () => {
    if (saveButton.disabled) return;
    handlers.onSave(gatewayInput.value, socks5Input.value);
  });
  restoreButton.addEventListener("click", () => {
    if (restoreButton.disabled) return;
    handlers.onRestoreDefaults();
  });

  const clearFeedback = () => {
    gatewayError.hidden = true;
    socks5Error.hidden = true;
    gatewayError.textContent = "";
    socks5Error.textContent = "";
    clear(feedback);
  };

  const showFieldError = (field: "gateway" | "socks5", message: string) => {
    const target = field === "gateway" ? gatewayError : socks5Error;
    const other = field === "gateway" ? socks5Error : gatewayError;
    target.textContent = message;
    target.hidden = false;
    other.hidden = true;
  };

  const showWarnings = (warnings: EndpointWarning[], source: "endpoints" | "proxy") => {
    clear(feedback);
    for (const warning of warnings) {
      feedback.append(el("p", { class: "endpoint-feedback-warn" }, [endpointWarningText(warning, source)]));
      if (warning === "keyRejected") {
        const reenter = el("button", { type: "button", class: "btn btn-sm" }, ["重新录入"]);
        reenter.addEventListener("click", () => handlers.onReenterKey());
        feedback.append(reenter);
      }
    }
  };

  const showMessage = (message: string) => {
    clear(feedback);
    feedback.append(el("p", { class: "endpoint-feedback-error" }, [message]));
  };

  const markClean = () => {
    gatewayDirty = false;
    socks5Dirty = false;
  };

  const update = (status: Status, busy: ReadonlySet<string>) => {
    const keepDraft =
      shouldKeepEndpointDraft({ focused: gatewayFocused, dirty: gatewayDirty }) ||
      shouldKeepEndpointDraft({ focused: socks5Focused, dirty: socks5Dirty });
    if (!keepDraft) {
      gatewayInput.value = status.gatewayBaseUrl;
      gatewayPreset.value = "";
      socks5Input.value = status.socks5Addr;
    }
    socks5Input.classList.toggle("endpoint-input-muted", socks5Muted(status.proxyEnabled));
    socks5Input.disabled = false;
    socks5Input.readOnly = false;

    const saving = busy.has("endpoints");
    saveButton.disabled = saving;
    saveButton.textContent = saving ? "保存中…" : "保存";
    restoreButton.disabled = !status.endpointsCustomized || saving;

    proxyToggle.update({
      enabled: status.proxyEnabled,
      busy: busy.has("proxy"),
      label: "启用本地代理",
      ariaLabel: "启用本地代理",
      onText: "已启用",
      offText: "已关闭",
      busyText: "切换中…",
    });
  };

  return { root, update, showFieldError, showWarnings, showMessage, markClean, clearFeedback };
}
