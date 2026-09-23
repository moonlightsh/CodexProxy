// 纯函数：状态 / 数值到界面文案与颜色等级的映射。不依赖 DOM，可在 Node 测试中直接运行。
import type { ProxyStatus, Reachability, Route, Status } from "./types";

/**
 * 首次运行判定（设计 §8）：未配置 Key 且未启用时，只显示简短说明与 Key 录入区，
 * 隐藏受管模式开关、状态灯、最近连接与底部区域。
 */
export function isFirstRun(status: Pick<Status, "keyConfigured" | "enabled">): boolean {
  return !status.keyConfigured && !status.enabled;
}

/**
 * Key 录入表单提交时应调用哪个命令（任务 3.2 要求 3）：
 * 已配置过 Key（无论当前是否启用）时是“重新录入”，调用 saveKey；
 * 尚未配置 Key（首次运行或 needsKey 场景）时调用 enable。
 */
export function keySubmitAction(status: Pick<Status, "keyConfigured">): "enable" | "saveKey" {
  return status.keyConfigured ? "saveKey" : "enable";
}

/** 状态灯的颜色等级；界面上始终配合文字展示，不只靠颜色区分（设计 §8）。 */
export type StatusLevel = "ok" | "warn" | "bad";

export function formatProxyStateLabel(proxy: ProxyStatus): string {
  switch (proxy.state) {
    case "running":
      return "运行中";
    case "external":
      return "已复用其他实例";
    case "stopped":
      return "未运行";
    default:
      return "未运行";
  }
}

export function proxyStateLevel(proxy: ProxyStatus): StatusLevel {
  return proxy.state === "stopped" ? "bad" : "ok";
}

export function formatLocalProxyAddr(port: number): string {
  return `127.0.0.1:${port}`;
}

export function formatReachabilityLabel(reachability: Reachability): string {
  switch (reachability) {
    case "reachable":
      return "可达";
    case "unreachable":
      return "不可达";
    case "unknown":
      return "检测中";
    default:
      return "检测中";
  }
}

export function reachabilityLevel(reachability: Reachability): StatusLevel {
  switch (reachability) {
    case "reachable":
      return "ok";
    case "unreachable":
      return "bad";
    case "unknown":
      return "warn";
    default:
      return "warn";
  }
}

/** `config.toml` / `.env` 接管情况的三态文案（设计 §8：已接管 / 未接管 / 部分接管）。 */
export function formatConfigManagedLabel(configManaged: boolean, envManaged: boolean): string {
  if (configManaged && envManaged) return "已接管";
  if (!configManaged && !envManaged) return "未接管";
  return "部分接管";
}

export function configManagedLevel(configManaged: boolean, envManaged: boolean): StatusLevel {
  if (configManaged && envManaged) return "ok";
  if (!configManaged && !envManaged) return "warn";
  return "bad";
}

export function formatRouteLabel(route: Route): string {
  return route === "socks5" ? "SOCKS5" : "直连";
}

export function formatOkMark(ok: boolean): string {
  return ok ? "✓" : "✗";
}

export function formatMs(ms: number): string {
  return `${ms}ms`;
}

export function formatHostPort(host: string, port: number): string {
  return `${host}:${port}`;
}
