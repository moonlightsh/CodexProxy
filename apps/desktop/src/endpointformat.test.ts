import assert from "node:assert/strict";
import { test } from "node:test";

import {
  endpointWarningText,
  formatSocks5Light,
  shouldKeepEndpointDraft,
  socks5Muted,
} from "./endpointformat.ts";

test("endpoint_warning_text_matches_design_copy", () => {
  assert.equal(
    endpointWarningText("keyRejected", "endpoints"),
    "当前 Key 在新网关校验失败（被拒绝），请重新录入 Key",
  );
  assert.equal(
    endpointWarningText("keyCheckUnavailable", "endpoints"),
    "无法连接新网关校验 Key，请检查地址或网络",
  );
  assert.equal(endpointWarningText("restartCodex", "endpoints"), "请重启 Codex 生效");
  assert.equal(
    endpointWarningText("externalProxy", "endpoints"),
    "本地代理由外部实例提供，新 SOCKS5 地址需重启该实例后生效",
  );
  assert.equal(
    endpointWarningText("externalProxy", "proxy"),
    "本地代理由外部实例提供，需要手动停止该实例",
  );
});

test("draft_is_kept_when_focused_or_dirty", () => {
  assert.equal(shouldKeepEndpointDraft({ focused: true, dirty: false }), true);
  assert.equal(shouldKeepEndpointDraft({ focused: false, dirty: true }), true);
  assert.equal(shouldKeepEndpointDraft({ focused: false, dirty: false }), false);
});

test("socks5_input_is_muted_only_when_proxy_is_off", () => {
  assert.equal(socks5Muted(false), true);
  assert.equal(socks5Muted(true), false);
});

test("socks5_light_says_disabled_when_proxy_is_off", () => {
  assert.deepEqual(formatSocks5Light(false, "reachable"), { text: "未启用", level: "warn" });
  assert.deepEqual(formatSocks5Light(true, "disabled"), { text: "未启用", level: "warn" });
  assert.deepEqual(formatSocks5Light(true, "unreachable"), { text: "不可达", level: "bad" });
});
