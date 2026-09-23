// 全局消息区：aria-live，展示操作结果与错误（任务 3.2 要求 6）。
import { el } from "./dom";

export type MessageKind = "info" | "success" | "error";

export interface MessageRegion {
  root: HTMLElement;
  show: (text: string, kind?: MessageKind) => void;
  clear: () => void;
}

const AUTO_HIDE_MS = 6000;

export function createMessageRegion(): MessageRegion {
  const root = el("div", { class: "message-region", "aria-live": "polite", role: "status" });
  let hideTimer: ReturnType<typeof setTimeout> | undefined;

  const clear = () => {
    root.textContent = "";
    root.removeAttribute("data-kind");
    root.hidden = true;
  };

  const show = (text: string, kind: MessageKind = "info") => {
    if (hideTimer) clearTimeout(hideTimer);
    root.textContent = text;
    root.setAttribute("data-kind", kind);
    root.hidden = false;
    if (kind !== "error") {
      hideTimer = setTimeout(clear, AUTO_HIDE_MS);
    }
  };

  clear();
  return { root, show, clear };
}
