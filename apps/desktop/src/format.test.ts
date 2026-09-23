import assert from "node:assert/strict";
import { test } from "node:test";

import {
  configManagedLevel,
  formatConfigManagedLabel,
  formatHostPort,
  formatLocalProxyAddr,
  formatMs,
  formatOkMark,
  formatProxyStateLabel,
  formatReachabilityLabel,
  formatRouteLabel,
  isFirstRun,
  keySubmitAction,
  proxyStateLevel,
  reachabilityLevel,
} from "./format.ts";

test("formatProxyStateLabel 覆盖三种代理状态", () => {
  assert.equal(formatProxyStateLabel({ state: "running", port: 17891 }), "运行中");
  assert.equal(formatProxyStateLabel({ state: "external", port: 17891 }), "已复用其他实例");
  assert.equal(formatProxyStateLabel({ state: "stopped", port: 17891 }), "未运行");
});

test("proxyStateLevel 只有 stopped 为 bad", () => {
  assert.equal(proxyStateLevel({ state: "running", port: 17891 }), "ok");
  assert.equal(proxyStateLevel({ state: "external", port: 17891 }), "ok");
  assert.equal(proxyStateLevel({ state: "stopped", port: 17891 }), "bad");
});

test("formatLocalProxyAddr 拼接回环地址", () => {
  assert.equal(formatLocalProxyAddr(17891), "127.0.0.1:17891");
});

test("formatReachabilityLabel / reachabilityLevel 覆盖三态", () => {
  assert.equal(formatReachabilityLabel("reachable"), "可达");
  assert.equal(formatReachabilityLabel("unreachable"), "不可达");
  assert.equal(formatReachabilityLabel("unknown"), "检测中");
  assert.equal(reachabilityLevel("reachable"), "ok");
  assert.equal(reachabilityLevel("unreachable"), "bad");
  assert.equal(reachabilityLevel("unknown"), "warn");
});

test("formatConfigManagedLabel 三态：已接管 / 未接管 / 部分接管", () => {
  assert.equal(formatConfigManagedLabel(true, true), "已接管");
  assert.equal(formatConfigManagedLabel(false, false), "未接管");
  assert.equal(formatConfigManagedLabel(true, false), "部分接管");
  assert.equal(formatConfigManagedLabel(false, true), "部分接管");
});

test("configManagedLevel 与文案三态对应", () => {
  assert.equal(configManagedLevel(true, true), "ok");
  assert.equal(configManagedLevel(false, false), "warn");
  assert.equal(configManagedLevel(true, false), "bad");
  assert.equal(configManagedLevel(false, true), "bad");
});

test("formatRouteLabel / formatOkMark / formatMs / formatHostPort", () => {
  assert.equal(formatRouteLabel("socks5"), "SOCKS5");
  assert.equal(formatRouteLabel("direct"), "直连");
  assert.equal(formatOkMark(true), "✓");
  assert.equal(formatOkMark(false), "✗");
  assert.equal(formatMs(120), "120ms");
  assert.equal(formatHostPort("chatgpt.com", 443), "chatgpt.com:443");
});

test("isFirstRun 仅在未配置 Key 且未启用时为 true", () => {
  assert.equal(isFirstRun({ keyConfigured: false, enabled: false }), true);
  assert.equal(isFirstRun({ keyConfigured: true, enabled: false }), false);
  assert.equal(isFirstRun({ keyConfigured: false, enabled: true }), false);
  assert.equal(isFirstRun({ keyConfigured: true, enabled: true }), false);
});

test("keySubmitAction 覆盖首次运行 / needsKey / 已配置已启用 / 已配置未启用四种状态", () => {
  // 首次运行：未配置且未启用 → enable
  assert.equal(keySubmitAction({ keyConfigured: false }), "enable");
  // needsKey：已启用但未配置 Key（keyConfigured 必为 false）→ enable
  assert.equal(keySubmitAction({ keyConfigured: false }), "enable");
  // 已配置且已启用：“重新录入” → saveKey
  assert.equal(keySubmitAction({ keyConfigured: true }), "saveKey");
  // 已配置但未启用：同样是“重新录入” → saveKey
  assert.equal(keySubmitAction({ keyConfigured: true }), "saveKey");
});
