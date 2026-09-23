// API Key 区域（设计 §8）：首次运行时的说明 + 录入区，以及常规模式下的
// “已配置 / 未配置” 状态行 + [重新录入] [清除 Key]。
//
// 安全约束（任务 3.2 要求 7）：输入框的值只在提交时被读取一次并立即清空，
// 不写入除 DOM 输入框以外的任何持久位置，本模块自身不保留 Key 副本。
import { clear, el } from "./dom";
import { isFirstRun } from "./format";
import type { Status } from "./types";

export interface KeyPanelHandlers {
  /** 表单提交：调用方（main.ts）据当前状态决定调用 enable 还是 saveKey。 */
  onSubmit: (key: string) => void;
  /** 点击“清除 Key”：调用方负责弹出确认模态框。 */
  onRequestClear: () => void;
}

export interface KeyPanelRegion {
  root: HTMLElement;
  update: (status: Status, busy: ReadonlySet<string>) => void;
  /** emptyKey / keyRejected：就地提示。 */
  showInlineError: (message: string) => void;
  /**
   * gatewayUnavailable：展示原因 + “仍然保存”按钮 + “取消”按钮。
   * onCancel 用于让用户主动丢弃暂存的 Key（调用方随之丢弃闭包中的 Key）。
   */
  showGatewayUnavailable: (message: string, onForceSave: () => void, onCancel: () => void) => void;
  /** “仍然保存”重试进行中：禁用按钮并显示“保存中…”，避免并发重复提交。 */
  setGatewayRetrying: (retrying: boolean) => void;
  /** 清除表单内的反馈信息（新一次提交前，或重试结束后调用）。 */
  resetFeedback: () => void;
  /** 展开录入表单并聚焦输入框（needsKey 场景）。 */
  openForm: () => void;
  /** 提交成功后收起表单（首次运行时表单始终展示，不受影响）。 */
  closeForm: () => void;
  focusInput: () => void;
}

const INTRO_LINES = [
  "CodexHelper 会：录入网关 API Key 并安全保存到系统凭据管理器；" +
    "接管 Codex 的 config.toml 与 .env，让 Codex 使用受管网关；" +
    "在本机常驻一个本地分流代理，把相关请求转发到上游 SOCKS5，其余直连。",
  "首次使用请先录入 API Key。",
];

export function createKeyPanel(handlers: KeyPanelHandlers): KeyPanelRegion {
  const intro = el(
    "div",
    { class: "key-intro" },
    INTRO_LINES.map((line) => el("p", {}, [line])),
  );

  const statusValue = el("span", { class: "key-status-value" }, ["未配置"]);
  const reenterButton = el("button", { type: "button", class: "btn btn-sm" }, ["重新录入"]);
  const clearButton = el("button", { type: "button", class: "btn btn-sm" }, ["清除 Key"]);
  const statusRow = el("div", { class: "key-status-row" }, [
    el("span", { class: "key-status-label" }, ["API Key"]),
    statusValue,
    reenterButton,
    clearButton,
  ]);

  const input = el("input", {
    type: "password",
    id: "key-input",
    class: "key-input",
    autocomplete: "off",
    spellcheck: "false",
    "aria-label": "API Key",
  }) as HTMLInputElement;
  const submitButton = el("button", { type: "submit", class: "btn btn-primary" }, ["提交"]);
  const cancelButton = el("button", { type: "button", class: "btn" }, ["取消"]);
  const feedback = el("div", { class: "key-feedback", "aria-live": "polite" });

  const form = el("form", { class: "key-form" }, [
    el("label", { for: "key-input", class: "key-form-label" }, ["API Key"]),
    input,
    el("div", { class: "key-form-actions" }, [submitButton, cancelButton]),
    feedback,
  ]) as HTMLFormElement;

  const root = el("div", { class: "key-panel" }, [intro, statusRow, form]);

  let formOpen = false;
  let firstRun = true;
  // needsKey 场景下表单会被强制展开（见 update()），此时“取消”按钮点了也会在下一次
  // update 时被重新展开，索性隐藏它，避免点击无效导致表单闪烁。
  let needsKeyState = false;
  // 当前反馈区展示的是否是 gatewayUnavailable 的“仍然保存”按钮，重试进行中据此禁用它。
  let forceSaveButton: HTMLButtonElement | null = null;
  let cancelForceSaveButton: HTMLButtonElement | null = null;

  const applyFormVisibility = () => {
    form.hidden = !(formOpen || firstRun);
    cancelButton.hidden = firstRun || needsKeyState;
  };

  const setFormOpen = (open: boolean) => {
    formOpen = open;
    applyFormVisibility();
  };

  const resetFeedback = () => {
    clear(feedback);
    feedback.hidden = true;
    forceSaveButton = null;
    cancelForceSaveButton = null;
  };

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const value = input.value;
    input.value = "";
    resetFeedback();
    handlers.onSubmit(value);
  });

  reenterButton.addEventListener("click", () => {
    resetFeedback();
    setFormOpen(true);
    input.focus();
  });

  cancelButton.addEventListener("click", () => {
    input.value = "";
    resetFeedback();
    setFormOpen(false);
  });

  clearButton.addEventListener("click", () => {
    handlers.onRequestClear();
  });

  const showInlineError = (message: string) => {
    clear(feedback);
    feedback.append(el("p", { class: "key-feedback-error" }, [message]));
    feedback.hidden = false;
  };

  const showGatewayUnavailable = (message: string, onForceSave: () => void, onCancel: () => void) => {
    clear(feedback);
    const button = el("button", { type: "button", class: "btn btn-sm" }, ["仍然保存"]) as HTMLButtonElement;
    button.addEventListener("click", onForceSave);
    const cancel = el("button", { type: "button", class: "btn btn-sm" }, ["取消"]) as HTMLButtonElement;
    cancel.addEventListener("click", onCancel);
    feedback.append(el("p", { class: "key-feedback-warn" }, [message]), button, cancel);
    feedback.hidden = false;
    forceSaveButton = button;
    cancelForceSaveButton = cancel;
  };

  const setGatewayRetrying = (retrying: boolean) => {
    if (!forceSaveButton) return;
    forceSaveButton.disabled = retrying;
    forceSaveButton.textContent = retrying ? "保存中…" : "仍然保存";
    if (cancelForceSaveButton) cancelForceSaveButton.disabled = retrying;
  };

  const update = (status: Status, busy: ReadonlySet<string>) => {
    firstRun = isFirstRun(status);
    intro.hidden = !firstRun;
    statusRow.hidden = firstRun;

    statusValue.textContent = status.keyConfigured ? "已配置" : "未配置";
    const clearing = busy.has("clearKey");
    clearButton.disabled = !status.keyConfigured || clearing;
    clearButton.textContent = clearing ? "清除中…" : "清除 Key";
    reenterButton.disabled = busy.has("saveKey") || busy.has("enable");

    needsKeyState = status.needsKey;
    if (status.needsKey) formOpen = true;
    applyFormVisibility();

    const submitting = busy.has("saveKey") || busy.has("enable");
    submitButton.disabled = submitting;
    submitButton.textContent = submitting ? "提交中…" : "提交";
    input.disabled = submitting;
  };

  resetFeedback();
  setFormOpen(false);
  return {
    root,
    update,
    showInlineError,
    showGatewayUnavailable,
    setGatewayRetrying,
    resetFeedback,
    openForm: () => setFormOpen(true),
    closeForm: () => setFormOpen(false),
    focusInput: () => input.focus(),
  };
}
