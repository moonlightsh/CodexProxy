import assert from "node:assert/strict";
import { test } from "node:test";

import {
  componentCheckButton,
  componentEmptyText,
  componentNetworkText,
  componentRowViewState,
} from "./componentformat.ts";
import type { ComponentUpdate, ComponentUpdates } from "./types";

// 契约示例（组件更新检查设计 §4.1 原样）：tsc 同时校验 TS 类型与 Rust 序列化形状一致。
const EXAMPLE: ComponentUpdates = {
  chatgptDesktop: {
    local: { kind: "installed", version: "26.1013.4894.0" },
    remote: { kind: "ok", version: "26.1013.4894.0", source: { kind: "desktopReleaseFeed" } },
    comparison: "upToDate",
    downloadUrl: null,
    viaProxy: true,
    directReason: null,
  },
  codexCli: {
    local: { kind: "installed", version: "0.162.1" },
    remote: {
      kind: "ok",
      version: "0.163.0",
      source: { kind: "codexLocalCache", checkedAt: "2026-10-16T05:53:49Z" },
      fallbackFrom: "github api 403 限流",
    },
    comparison: "updateAvailable",
    downloadUrl: "https://github.com/openai/codex/releases/tag/rust-v0.163.0",
    viaProxy: false,
    directReason: "代理为外部实例，模式未知，已直连",
  },
};

/** 以“已安装 + 远端可用 + 未知比较、直连无原因”为底，按需覆盖字段。 */
function row(patch: Partial<ComponentUpdate>): ComponentUpdate {
  return {
    local: { kind: "installed", version: "0.162.1" },
    remote: { kind: "ok", version: "0.163.0", source: { kind: "githubApi" } },
    comparison: "unknown",
    downloadUrl: null,
    viaProxy: false,
    directReason: null,
    ...patch,
  };
}

test("契约示例：ChatGPT 已是最新、CLI 缓存兜底有更新", () => {
  const desktop = componentRowViewState("chatgptDesktop", EXAMPLE.chatgptDesktop);
  assert.equal(desktop.label, "ChatGPT Desktop");
  assert.equal(desktop.localText, "26.1013.4894.0");
  assert.equal(desktop.remoteText, "26.1013.4894.0");
  assert.deepEqual(desktop.badge, { text: "已是最新", level: "ok" });
  assert.equal(desktop.note, null);
  assert.equal(desktop.download, null);

  const cli = componentRowViewState("codexCli", EXAMPLE.codexCli);
  assert.equal(cli.label, "Codex CLI（独立安装）");
  assert.equal(cli.badge.text, "有新版本 v0.163.0");
  assert.equal(cli.badge.level, "warn");
  assert.equal(cli.note, "远端不可达（github api 403 限流），以下为 Codex 本地缓存，检查于 2026-10-16T05:53:49Z");
  assert.equal(cli.download?.url, "https://github.com/openai/codex/releases/tag/rust-v0.163.0");
  assert.equal(cli.download?.hint, "请通过离线包管线更新 Codex CLI。发布页：");

  assert.equal(componentNetworkText(EXAMPLE), "经本地代理检测");
});

test("Secure 未就绪：远端为 detail、徽标 bad、网络说明零出站", () => {
  const blocked = row({ remote: { kind: "blocked", detail: "Secure 代理未就绪（代理未运行）" } });
  const view = componentRowViewState("codexCli", blocked);
  assert.equal(view.remoteText, "Secure 代理未就绪（代理未运行）");
  assert.deepEqual(view.badge, { text: "Secure 代理未就绪", level: "bad" });
  assert.equal(view.download, null);
  assert.equal(
    componentNetworkText({ chatgptDesktop: blocked, codexCli: blocked }),
    "未发出远端请求（Secure 代理未就绪）",
  );
});

test("开发版 0.0.0 不比较", () => {
  const view = componentRowViewState(
    "codexCli",
    row({ local: { kind: "installed", version: "0.0.0" }, downloadUrl: "https://example.invalid/x" }),
  );
  assert.deepEqual(view.badge, { text: "开发版，不比较", level: "warn" });
  assert.equal(view.download, null);
  // 预发布形态的开发版同样不比较
  const pre = componentRowViewState("codexCli", row({ local: { kind: "installed", version: "0.0.0-dev" } }));
  assert.equal(pre.badge.text, "开发版，不比较");
  // 开发版规则只适用于 CLI
  const desktop = componentRowViewState("chatgptDesktop", row({ local: { kind: "installed", version: "0.0.0" } }));
  assert.equal(desktop.badge.text, "未知");
});

test("本地状态文案", () => {
  const cases: Array<[ComponentUpdate["local"], string, { text: string; level: string }]> = [
    [{ kind: "notInstalled" }, "未安装", { text: "未安装", level: "warn" }],
    [
      { kind: "notRegisteredForUser" },
      "未安装或当前用户未注册（预配安装后可能需重新登录）",
      { text: "未注册", level: "warn" },
    ],
    [
      { kind: "probeFailed", detail: "超过 15 秒未完成" },
      "探测失败：超过 15 秒未完成",
      { text: "探测失败", level: "bad" },
    ],
    [{ kind: "unsupportedPlatform" }, "暂不支持（二期）", { text: "暂不支持（二期）", level: "warn" }],
  ];
  for (const [local, localText, badge] of cases) {
    const view = componentRowViewState("chatgptDesktop", row({ local }));
    assert.equal(view.localText, localText);
    assert.deepEqual(view.badge, badge);
  }
  const unsupported = componentRowViewState("chatgptDesktop", row({ local: { kind: "unsupportedPlatform" }, remote: { kind: "unsupportedPlatform" } }));
  assert.equal(unsupported.remoteText, "暂不支持（二期）");
  assert.deepEqual(unsupported.badge, { text: "暂不支持（二期）", level: "warn" });
});

test("远端失败文案", () => {
  const unreachable = componentRowViewState("codexCli", row({ remote: { kind: "unreachable", detail: "超时" } }));
  assert.equal(unreachable.remoteText, "远端不可达：超时");
  assert.deepEqual(unreachable.badge, { text: "远端不可达", level: "bad" });

  const invalid = componentRowViewState(
    "chatgptDesktop",
    row({ remote: { kind: "invalid", detail: "下载地址不在官方白名单内" } }),
  );
  assert.equal(invalid.remoteText, "清单无效：下载地址不在官方白名单内");
  assert.deepEqual(invalid.badge, { text: "清单无效", level: "bad" });
  assert.equal(invalid.note, null);
});

test("缓存兜底缺失远端原因时说明原因未知", () => {
  const view = componentRowViewState(
    "codexCli",
    row({ remote: { kind: "ok", version: "0.163.0", source: { kind: "codexLocalCache", checkedAt: "2026-10-16T05:53:49Z" } } }),
  );
  assert.equal(view.note, "远端不可达（原因未知），以下为 Codex 本地缓存，检查于 2026-10-16T05:53:49Z");
});

test("本地较新不提示下载", () => {
  const view = componentRowViewState(
    "codexCli",
    row({ comparison: "localNewer", downloadUrl: "https://github.com/openai/codex/releases/tag/rust-v0.163.0" }),
  );
  assert.deepEqual(view.badge, { text: "本地较新", level: "ok" });
  assert.equal(view.download, null);
});

test("ChatGPT 有更新引导离线包管线", () => {
  const url = "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix";
  const view = componentRowViewState(
    "chatgptDesktop",
    row({
      local: { kind: "installed", version: "26.1012.1.0" },
      remote: { kind: "ok", version: "26.1013.4894.0", source: { kind: "desktopReleaseFeed" } },
      comparison: "updateAvailable",
      downloadUrl: url,
    }),
  );
  assert.equal(view.badge.text, "有新版本 v26.1013.4894.0");
  assert.ok(view.download?.hint.includes("ChatGPT-License.xml"));
  assert.equal(view.download?.hint, "请通过离线包管线更新（离线安装还需 ChatGPT-License.xml）。官方安装包地址：");
  assert.equal(view.download?.url, url);
});

test("有更新但缺下载地址时不展示下载区", () => {
  const view = componentRowViewState("codexCli", row({ comparison: "updateAvailable", downloadUrl: null }));
  assert.equal(view.badge.text, "有新版本 v0.163.0");
  assert.equal(view.download, null);
});

test("直连说明带原因", () => {
  const direct = row({ directReason: "代理为外部实例，模式未知，已直连" });
  assert.equal(
    componentNetworkText({ chatgptDesktop: direct, codexCli: direct }),
    "直连检测：代理为外部实例，模式未知，已直连",
  );
  const plain = row({});
  assert.equal(componentNetworkText({ chatgptDesktop: plain, codexCli: plain }), "直连检测");
});

test("检查按钮状态", () => {
  assert.deepEqual(componentCheckButton(true), { text: "检查中…", disabled: true });
  assert.deepEqual(componentCheckButton(false), { text: "检查组件更新", disabled: false });
});

test("尚无结果时的占位文案：检查中如实提示最坏耗时", () => {
  // 最坏：本地探测 15s（macOS 另加登录 shell 5s）+ API 15s + 302 回退 15s ≈ 50s
  const checking = componentEmptyText(true);
  assert.match(checking, /1 分钟/);
  assert.ok(!checking.includes("十几秒"));
  assert.equal(componentEmptyText(false), "点击“检查组件更新”查看 ChatGPT Desktop 与 Codex CLI 的最新版本。");
});
