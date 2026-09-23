import assert from "node:assert/strict";
import { test } from "node:test";

import { MAX_RECENT_CONNECTIONS, truncateConnections, withNewConnection } from "./connections.ts";
import type { ConnectionRecord } from "./types.ts";

function record(host: string, time = 0): ConnectionRecord {
  return { time, host, port: 443, decision: "direct", ok: true, ms: 1 };
}

test("withNewConnection 插入到头部", () => {
  const list = [record("a"), record("b")];
  const next = withNewConnection(list, record("c"));
  assert.deepEqual(
    next.map((r) => r.host),
    ["c", "a", "b"],
  );
  // 不修改原数组
  assert.deepEqual(
    list.map((r) => r.host),
    ["a", "b"],
  );
});

test("withNewConnection 截断到最多 50 条", () => {
  const list = Array.from({ length: MAX_RECENT_CONNECTIONS }, (_, i) => record(`h${i}`));
  const next = withNewConnection(list, record("new"));
  assert.equal(next.length, MAX_RECENT_CONNECTIONS);
  assert.equal(next[0]?.host, "new");
  // 最后一条被挤出
  assert.ok(!next.some((r) => r.host === `h${MAX_RECENT_CONNECTIONS - 1}`));
});

test("truncateConnections 截断超长的初始列表", () => {
  const list = Array.from({ length: MAX_RECENT_CONNECTIONS + 10 }, (_, i) => record(`h${i}`));
  const next = truncateConnections(list);
  assert.equal(next.length, MAX_RECENT_CONNECTIONS);
  assert.equal(next[0]?.host, "h0");
});
