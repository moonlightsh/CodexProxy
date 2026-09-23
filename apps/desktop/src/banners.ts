// 横幅区：按需显示（设计 §8，任务 3.2 要求 1）。
import { clear, el } from "./dom";
import type { Status } from "./types";

export interface BannerHandlers {
  onCleanupResidue: () => void;
  onRemoveLegacyEnvBlock: () => void;
}

export interface BannerRegion {
  root: HTMLElement;
  update: (status: Status, busy: ReadonlySet<string>) => void;
}

export function createBannerRegion(handlers: BannerHandlers): BannerRegion {
  const root = el("div", { class: "banners" });

  const update = (status: Status, busy: ReadonlySet<string>) => {
    clear(root);
    const banners: HTMLElement[] = [];

    if (!status.platformSupported) {
      banners.push(
        el("div", { class: "banner banner-dev" }, [
          "开发模式：当前平台不受支持，凭据使用进程内内存实现，仅供调试。",
        ]),
      );
    }

    if (status.needsKey) {
      banners.push(
        el("div", { class: "banner banner-warn" }, [
          "已启用但未录入 API Key，Codex 请求将失败。",
        ]),
      );
    }

    if (status.residue) {
      const cleaning = busy.has("cleanupResidue");
      const button = el(
        "button",
        { type: "button", class: "btn btn-sm", disabled: cleaning || undefined },
        [cleaning ? "清理中…" : "一键清理"],
      );
      button.addEventListener("click", handlers.onCleanupResidue);
      banners.push(
        el("div", { class: "banner banner-warn" }, ["检测到受管配置残留。", button]),
      );
    }

    if (status.legacyEnvBlock) {
      const removing = busy.has("removeLegacyEnvBlock");
      const button = el(
        "button",
        { type: "button", class: "btn btn-sm", disabled: removing || undefined },
        [removing ? "移除中…" : "一键移除"],
      );
      button.addEventListener("click", handlers.onRemoveLegacyEnvBlock);
      banners.push(
        el("div", { class: "banner banner-warn" }, [
          ".env 中存在 Codex++ 旧的受管块。",
          button,
        ]),
      );
    }

    if (status.configError) {
      banners.push(
        el("div", { class: "banner banner-error" }, [
          el("span", {}, [status.configError]),
          el("span", {}, ["请先修复 config.toml 后再试。"]),
        ]),
      );
    }

    for (const banner of banners) root.append(banner);
    root.hidden = banners.length === 0;
  };

  root.hidden = true;
  return { root, update };
}
