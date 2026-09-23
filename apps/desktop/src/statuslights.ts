// 状态灯四行（设计 §8）：颜色 + 文字，不只靠颜色区分。
import { clear, el } from "./dom";
import {
  configManagedLevel,
  formatConfigManagedLabel,
  formatLocalProxyAddr,
  formatProxyStateLabel,
  formatReachabilityLabel,
  proxyStateLevel,
  reachabilityLevel,
  type StatusLevel,
} from "./format";
import type { Status } from "./types";

function lightRow(label: string, addr: string, valueText: string, level: StatusLevel): HTMLElement {
  return el("div", { class: `status-light status-light-${level}` }, [
    el("span", { class: "status-dot", "aria-hidden": "true" }),
    el("span", { class: "status-light-label" }, [label]),
    el("span", { class: "status-light-addr" }, [addr]),
    el("span", { class: "status-light-value" }, [valueText]),
  ]);
}

export interface StatusLightsRegion {
  root: HTMLElement;
  update: (status: Status) => void;
}

export function createStatusLightsRegion(): StatusLightsRegion {
  const root = el("div", { class: "status-lights" });

  const update = (status: Status) => {
    clear(root);
    root.append(
      lightRow(
        "本地代理",
        formatLocalProxyAddr(status.proxy.port),
        formatProxyStateLabel(status.proxy),
        proxyStateLevel(status.proxy),
      ),
      lightRow(
        "模型网关",
        status.gatewayAddr,
        formatReachabilityLabel(status.gateway),
        reachabilityLevel(status.gateway),
      ),
      lightRow(
        "SOCKS5",
        status.socks5Addr,
        formatReachabilityLabel(status.socks5),
        reachabilityLevel(status.socks5),
      ),
      lightRow(
        "Codex 配置",
        "config.toml / .env",
        formatConfigManagedLabel(status.configManaged, status.envManaged),
        configManagedLevel(status.configManaged, status.envManaged),
      ),
    );
  };

  return { root, update };
}
