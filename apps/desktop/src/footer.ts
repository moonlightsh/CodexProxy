// 底部：开机自启复选框与提示文案（设计 §8）。
import { el } from "./dom";
import type { Status } from "./types";

export interface FooterHandlers {
  onToggleAutostart: (enabled: boolean) => void;
}

export interface FooterRegion {
  root: HTMLElement;
  update: (status: Status, busy: boolean) => void;
}

export function createFooterRegion(handlers: FooterHandlers): FooterRegion {
  const checkbox = el("input", { type: "checkbox", id: "autostart-checkbox" }) as HTMLInputElement;
  const checkboxLabel = el("label", { for: "autostart-checkbox" }, ["开机自启"]);
  const hint = el("p", { class: "footer-hint" }, [
    "修改后请重启 Codex；受管模式需要本工具保持运行。",
  ]);
  const root = el("div", { class: "footer-region" }, [
    el("div", { class: "autostart-row" }, [checkbox, checkboxLabel]),
    hint,
  ]);

  checkbox.addEventListener("change", () => {
    handlers.onToggleAutostart(checkbox.checked);
  });

  const update = (status: Status, busy: boolean) => {
    checkbox.checked = status.autostart;
    checkbox.disabled = busy;
  };

  return { root, update };
}
