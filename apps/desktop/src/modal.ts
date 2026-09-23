// 自定义确认模态框：替代 window.alert / confirm / prompt（任务 3.2 要求 4）。
import { el } from "./dom";

export interface ConfirmModalOptions {
  title: string;
  /** 说明文字，可多段。 */
  message: string[];
  confirmText: string;
  cancelText: string;
  /** 危险操作（如清除 Key）用醒目样式标记确认按钮。 */
  danger?: boolean;
}

/** 展示确认模态框，返回用户是否点击了确认。Esc 或点击遮罩视为取消。 */
export function confirmModal(options: ConfirmModalOptions): Promise<boolean> {
  return new Promise((resolve) => {
    const titleId = `modal-title-${Math.random().toString(36).slice(2)}`;

    let settled = false;
    const finish = (result: boolean) => {
      if (settled) return;
      settled = true;
      document.removeEventListener("keydown", onKeyDown);
      overlay.remove();
      resolve(result);
    };

    // 简单的 Tab 焦点循环：模态框内只有取消 / 确认两个可聚焦元素，循环切换即可，
    // 避免 Tab 把焦点移到遮罩后面的开关等控件上。
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        finish(false);
        return;
      }
      if (event.key !== "Tab") return;
      const focusables = [cancelButton, confirmButton];
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (event.shiftKey) {
        if (document.activeElement === first) {
          event.preventDefault();
          last.focus();
        }
      } else if (document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    const cancelButton = el("button", { type: "button", class: "btn" }, [options.cancelText]);
    cancelButton.addEventListener("click", () => finish(false));

    const confirmButton = el(
      "button",
      { type: "button", class: options.danger ? "btn btn-danger" : "btn btn-primary" },
      [options.confirmText],
    );
    confirmButton.addEventListener("click", () => finish(true));

    const body = options.message.map((line) => el("p", {}, [line]));

    const dialog = el(
      "div",
      { class: "modal-dialog", role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
      [
        el("h2", { id: titleId, class: "modal-title" }, [options.title]),
        el("div", { class: "modal-body" }, body),
        el("div", { class: "modal-actions" }, [cancelButton, confirmButton]),
      ],
    );
    dialog.addEventListener("click", (event) => event.stopPropagation());

    const overlay = el("div", { class: "modal-overlay" }, [dialog]);
    overlay.addEventListener("click", () => finish(false));

    document.body.append(overlay);
    document.addEventListener("keydown", onKeyDown);
    // 危险操作（如清除 Key）默认聚焦“取消”，避免一按 Enter 就执行破坏性操作。
    (options.danger ? cancelButton : confirmButton).focus();
  });
}
