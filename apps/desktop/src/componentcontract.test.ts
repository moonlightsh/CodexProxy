// 组件更新检查跨端契约（设计 §4.1）：BEGIN/END 标记之间是严格 JSON，
// 同一份夹具被 crates/helper-core/src/component_updates.rs 的
// serialization_matches_frontend_contract_fixture 以 include_str! 读取，断言 Rust 序列化结果逐项相等；
// 这里以 ComponentUpdates[] 类型声明，tsc 校验其与 types.ts 一致（多/缺字段、kind 拼写都会报错）。
// 修改任一端的形状都必须同步本夹具，否则两端测试之一会失败。
import assert from "node:assert/strict";
import { test } from "node:test";

import { componentNetworkText, componentRowViewState } from "./componentformat.ts";
import type { ComponentUpdates } from "./types";

const CASES: ComponentUpdates[] = /* CONTRACT-JSON-BEGIN */ [
  {
    "chatgptDesktop": {
      "local": {
        "kind": "installed",
        "version": "26.1012.1.0"
      },
      "remote": {
        "kind": "ok",
        "version": "26.1013.4894.0",
        "source": {
          "kind": "desktopReleaseFeed"
        }
      },
      "comparison": "updateAvailable",
      "downloadUrl": "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix",
      "viaProxy": true,
      "directReason": null
    },
    "codexCli": {
      "local": {
        "kind": "installed",
        "version": "0.162.1"
      },
      "remote": {
        "kind": "ok",
        "version": "0.163.0",
        "source": {
          "kind": "codexLocalCache",
          "checkedAt": "2026-10-16T05:53:49Z"
        },
        "fallbackFrom": "github api 403 限流；302 回退失败：超时"
      },
      "comparison": "updateAvailable",
      "downloadUrl": "https://github.com/openai/codex/releases/tag/rust-v0.163.0",
      "viaProxy": true,
      "directReason": null
    }
  },
  {
    "chatgptDesktop": {
      "local": {
        "kind": "notRegisteredForUser"
      },
      "remote": {
        "kind": "ok",
        "version": "26.1013.4894.0",
        "source": {
          "kind": "desktopReleaseFeed"
        }
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": "本地代理未运行，已直连"
    },
    "codexCli": {
      "local": {
        "kind": "notInstalled"
      },
      "remote": {
        "kind": "ok",
        "version": "0.163.0",
        "source": {
          "kind": "githubRedirect"
        }
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": "本地代理未运行，已直连"
    }
  },
  {
    "chatgptDesktop": {
      "local": {
        "kind": "probeFailed",
        "detail": "超过 15 秒未完成"
      },
      "remote": {
        "kind": "invalid",
        "detail": "清单格式无效：下载地址不在官方白名单内"
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": true,
      "directReason": null
    },
    "codexCli": {
      "local": {
        "kind": "installed",
        "version": "0.164.0"
      },
      "remote": {
        "kind": "ok",
        "version": "0.163.0",
        "source": {
          "kind": "githubApi"
        }
      },
      "comparison": "localNewer",
      "downloadUrl": null,
      "viaProxy": true,
      "directReason": null
    }
  },
  {
    "chatgptDesktop": {
      "local": {
        "kind": "installed",
        "version": "26.1013.4894.0"
      },
      "remote": {
        "kind": "blocked",
        "detail": "Secure 代理未就绪（代理未运行）"
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": null
    },
    "codexCli": {
      "local": {
        "kind": "installed",
        "version": "0.162.1"
      },
      "remote": {
        "kind": "blocked",
        "detail": "Secure 代理未就绪（代理未运行）"
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": null
    }
  },
  {
    "chatgptDesktop": {
      "local": {
        "kind": "unsupportedPlatform"
      },
      "remote": {
        "kind": "unsupportedPlatform"
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": null
    },
    "codexCli": {
      "local": {
        "kind": "installed",
        "version": "0.163.0"
      },
      "remote": {
        "kind": "ok",
        "version": "0.163.0",
        "source": {
          "kind": "githubApi"
        }
      },
      "comparison": "upToDate",
      "downloadUrl": null,
      "viaProxy": true,
      "directReason": null
    }
  },
  {
    "chatgptDesktop": {
      "local": {
        "kind": "installed",
        "version": "26.1013.4894.0"
      },
      "remote": {
        "kind": "ok",
        "version": "26.1013.4894.0",
        "source": {
          "kind": "desktopReleaseFeed"
        }
      },
      "comparison": "upToDate",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": "代理为外部实例，模式未知，已直连"
    },
    "codexCli": {
      "local": {
        "kind": "installed",
        "version": "0.0.0"
      },
      "remote": {
        "kind": "unreachable",
        "detail": "github api 连接失败"
      },
      "comparison": "unknown",
      "downloadUrl": null,
      "viaProxy": false,
      "directReason": "代理为外部实例，模式未知，已直连"
    }
  }
] /* CONTRACT-JSON-END */;

/** 每条夹具的期望徽标（ChatGPT, CLI）与网络说明：确认前端能消费 Rust 实际产出的每种形状。 */
const EXPECTED: Array<[string, string, string]> = [
  [
    "有新版本 v26.1013.4894.0",
    "有新版本 v0.163.0",
    "经本地代理检测"
  ],
  [
    "未注册",
    "未安装",
    "直连检测：本地代理未运行，已直连"
  ],
  [
    "清单无效",
    "本地较新",
    "经本地代理检测"
  ],
  [
    "Secure 代理未就绪",
    "Secure 代理未就绪",
    "未发出远端请求（Secure 代理未就绪）"
  ],
  [
    "暂不支持（二期）",
    "已是最新",
    "经本地代理检测"
  ],
  [
    "已是最新",
    "开发版，不比较",
    "直连检测：代理为外部实例，模式未知，已直连"
  ]
];

test("契约夹具覆盖全部变体且前端逐条可渲染", () => {
  assert.equal(CASES.length, EXPECTED.length);
  CASES.forEach((result, index) => {
    const [desktopBadge, cliBadge, network] = EXPECTED[index];
    assert.equal(componentRowViewState("chatgptDesktop", result.chatgptDesktop).badge.text, desktopBadge);
    assert.equal(componentRowViewState("codexCli", result.codexCli).badge.text, cliBadge);
    assert.equal(componentNetworkText(result), network);
  });
  const kinds = new Set(CASES.flatMap((r) => [r.chatgptDesktop, r.codexCli]).flatMap((row) => [
    `local:${row.local.kind}`,
    `remote:${row.remote.kind}`,
    `comparison:${row.comparison}`,
    ...(row.remote.kind === "ok" ? [`source:${row.remote.source.kind}`] : []),
  ]));
  for (const kind of [
    "local:installed", "local:notInstalled", "local:notRegisteredForUser", "local:probeFailed", "local:unsupportedPlatform",
    "remote:ok", "remote:unreachable", "remote:invalid", "remote:blocked", "remote:unsupportedPlatform",
    "source:desktopReleaseFeed", "source:githubApi", "source:githubRedirect", "source:codexLocalCache",
    "comparison:updateAvailable", "comparison:upToDate", "comparison:localNewer", "comparison:unknown",
  ]) {
    assert.ok(kinds.has(kind), `夹具缺少 ${kind}`);
  }
});
