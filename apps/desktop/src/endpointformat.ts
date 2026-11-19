// 网关设置的纯函数：警告文案、编辑中是否保留草稿、SOCKS5 置灰与状态灯。
import { formatReachabilityLabel, reachabilityLevel, type StatusLevel } from "./format.ts";
import type { EndpointWarning, Reachability } from "./types.ts";

/** 保存端点或切换代理后的非致命提示。`externalProxy` 按触发操作选文案。 */
export function endpointWarningText(
  warning: EndpointWarning,
  source: "endpoints" | "proxy",
): string {
  switch (warning) {
    case "keyRejected":
      return "当前 Key 在新网关校验失败（被拒绝），请重新录入 Key";
    case "keyCheckUnavailable":
      return "无法连接新网关校验 Key，请检查地址或网络";
    case "restartCodex":
      return "请重启 Codex 生效";
    case "externalProxy":
      return source === "proxy"
        ? "本地代理由外部实例提供，需要手动停止该实例"
        : "本地代理由外部实例提供，新 SOCKS5 地址需重启该实例后生效";
  }
}

/** 输入框有焦点或内容已改动时，状态刷新不覆盖草稿。 */
export function shouldKeepEndpointDraft(input: { focused: boolean; dirty: boolean }): boolean {
  return input.focused || input.dirty;
}

/** 代理关闭时 SOCKS5 输入框只降低不透明度，仍可编辑。 */
export function socks5Muted(proxyEnabled: boolean): boolean {
  return !proxyEnabled;
}

/**
 * SOCKS5 状态灯。代理关闭或可达性为 disabled 时显示「未启用」；
 * 其余原样使用可达性文案与等级。
 */
export function formatSocks5Light(
  proxyEnabled: boolean,
  reachability: Reachability,
): { text: string; level: StatusLevel } {
  if (!proxyEnabled || reachability === "disabled") {
    return { text: "未启用", level: "warn" };
  }
  return { text: formatReachabilityLabel(reachability), level: reachabilityLevel(reachability) };
}
