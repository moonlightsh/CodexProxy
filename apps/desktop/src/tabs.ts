// 顶部标签页（WAI-ARIA Tabs 模式）：方向键 / Home / End 切换，选中即激活。
import { el } from "./dom";

export interface TabDef<Id extends string> {
  id: Id;
  label: string;
  panel: HTMLElement;
}

export interface TabsRegion<Id extends string> {
  root: HTMLElement;
  select: (id: Id) => void;
}

export function createTabs<Id extends string>(
  tabs: Array<TabDef<Id>>,
  onChange: (id: Id) => void,
): TabsRegion<Id> {
  const root = el("div", { class: "tabs", role: "tablist", "aria-label": "页面" });
  const buttons = tabs.map((tab) => {
    const tabId = `tab-${tab.id}`;
    const panelId = `panel-${tab.id}`;
    tab.panel.id = panelId;
    tab.panel.setAttribute("role", "tabpanel");
    tab.panel.setAttribute("aria-labelledby", tabId);
    const button = el("button", { type: "button", class: "tab", role: "tab", id: tabId, "aria-controls": panelId }, [
      tab.label,
    ]) as HTMLButtonElement;
    button.addEventListener("click", () => activate(tab.id));
    root.append(button);
    return button;
  });

  let current: Id | null = null;

  const select = (id: Id) => {
    current = id;
    tabs.forEach((tab, index) => {
      const selected = tab.id === id;
      const button = buttons[index];
      if (button) {
        button.setAttribute("aria-selected", selected ? "true" : "false");
        button.tabIndex = selected ? 0 : -1;
      }
      tab.panel.hidden = !selected;
    });
  };

  const activate = (id: Id) => {
    if (id === current) return;
    select(id);
    onChange(id);
  };

  root.addEventListener("keydown", (event) => {
    const index = tabs.findIndex((tab) => tab.id === current);
    if (index < 0) return;
    let next: number;
    switch (event.key) {
      case "ArrowRight":
        next = (index + 1) % tabs.length;
        break;
      case "ArrowLeft":
        next = (index - 1 + tabs.length) % tabs.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = tabs.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    const tab = tabs[next];
    if (!tab) return;
    activate(tab.id);
    buttons[next]?.focus();
  });

  return { root, select };
}
