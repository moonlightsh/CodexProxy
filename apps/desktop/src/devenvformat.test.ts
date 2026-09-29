import assert from "node:assert/strict";
import { test } from "node:test";

import {
  devEnvRows,
  formatClock,
  hostOf,
  NODE_DOWNLOAD_URL,
  pipIniSnippet,
  PYTHON_DOWNLOAD_URL,
} from "./devenvformat.ts";
import type { DevEnvReport, MirrorReport, RuntimeReport } from "./types";

const PIP = "http://mirrors.aliyun.com/pypi/simple/";
const NPM = "https://registry.npmmirror.com/";

function runtime(patch: Partial<RuntimeReport>): RuntimeReport {
  return { state: "ok", path: null, version: null, minVersion: null, detail: null, ...patch };
}

function mirror(expected: string, patch: Partial<MirrorReport>): MirrorReport {
  return { state: "configured", current: expected, expected, detail: null, ...patch };
}

function report(patch: Partial<DevEnvReport>): DevEnvReport {
  return {
    platformSupported: true,
    python: runtime({ path: "C:\\Python313\\python.exe", version: "3.13.14", minVersion: "3.13" }),
    pipMirror: mirror(PIP, {}),
    node: runtime({ path: "C:\\Tools\\nodejs\\node.exe", version: "24.16.0" }),
    npmMirror: mirror(NPM, {}),
    ...patch,
  };
}

test("全部满足时四行均为 ok 且无提示", () => {
  const rows = devEnvRows(report({}));
  assert.deepEqual(
    rows.map((row) => [row.label, row.value, row.level]),
    [
      ["Python", "3.13.14", "ok"],
      ["pip 镜像", "已启用", "ok"],
      ["Node.js", "24.16.0", "ok"],
      ["npm 镜像", "已启用", "ok"],
    ],
  );
  assert.ok(rows.every((row) => row.hint === null));
  assert.equal(rows[0]?.addr, "C:\\Python313\\python.exe");
});

test("Python 未安装 / 版本过低给出内网下载地址，镜像行为未检测", () => {
  const missing = devEnvRows(
    report({
      python: runtime({ state: "missing", minVersion: "3.13" }),
      pipMirror: mirror(PIP, { state: "skipped", current: null }),
    }),
  );
  assert.equal(missing[0]?.value, "未安装");
  assert.equal(missing[0]?.addr, "PATH 中未找到 python");
  assert.equal(missing[0]?.hint?.code, PYTHON_DOWNLOAD_URL);
  assert.ok(missing[0]?.hint?.text.includes("Python 3.13+"));
  assert.deepEqual([missing[1]?.value, missing[1]?.level, missing[1]?.hint], ["未检测", "warn", null]);

  const old = devEnvRows(report({ python: runtime({ state: "tooOld", version: "3.12.9", minVersion: "3.13" }) }));
  assert.equal(old[0]?.value, "版本过低（3.12.9，需要 3.13+）");
  assert.equal(old[0]?.level, "bad");
  assert.equal(old[0]?.hint?.code, PYTHON_DOWNLOAD_URL);
  assert.ok(old[0]?.hint?.text.includes("在 Path 中把新版本目录移到旧版本之前"));
  assert.ok(!missing[0]?.hint?.text.includes("旧版本"));
});

test("Store 占位程序与无法运行给出对应说明", () => {
  const stub = devEnvRows(report({ python: runtime({ state: "storeStub", path: "C:\\x\\WindowsApps\\python.exe" }) }));
  assert.equal(stub[0]?.value, "Microsoft Store 占位程序");
  assert.ok(stub[0]?.hint?.text.includes("安装后如仍检测到占位程序"));
  assert.ok(stub[0]?.hint?.text.includes("应用执行别名”中关闭 python.exe"));
  assert.equal(stub[0]?.hint?.code, PYTHON_DOWNLOAD_URL);

  const broken = devEnvRows(report({ node: runtime({ state: "broken", detail: "node --version 超时" }) }));
  assert.deepEqual([broken[2]?.value, broken[2]?.hint?.text, broken[2]?.hint?.code], [
    "无法运行",
    "node --version 超时",
    null,
  ]);
  const missingNode = devEnvRows(report({ node: runtime({ state: "missing" }) }));
  assert.equal(missingNode[2]?.hint?.code, NODE_DOWNLOAD_URL);
  assert.ok(missingNode[2]?.hint?.text.includes("改名为 C:\\Tools\\nodejs"));
  assert.ok(missingNode[2]?.hint?.text.includes("C:\\Tools\\nodejs\\node.exe"));
});

test("下载地址与内网文档一致（https）", () => {
  assert.equal(PYTHON_DOWNLOAD_URL, "https://rdc.tiandy.com/nexus/repository/cypub/tools/python-3.13.14-amd64.exe");
  assert.equal(NODE_DOWNLOAD_URL, "https://rdc.tiandy.com/nexus/repository/cypub/tools/node-v24.16.0-win-x64.zip");
});

test("pip 未启用给出 pip.ini 内容，默认源显示 pypi.org", () => {
  const rows = devEnvRows(report({ pipMirror: mirror(PIP, { state: "notConfigured", current: null }) }));
  assert.equal(rows[1]?.addr, "默认源（pypi.org）");
  assert.equal(rows[1]?.level, "bad");
  assert.equal(rows[1]?.hint?.code, pipIniSnippet(PIP));
  assert.ok(rows[1]?.hint?.text.includes("替换 pip.ini 中原有的 [global] / [install] 节"));
  assert.ok(rows[1]?.hint?.text.includes("%APPDATA%\\pip\\pip.ini"));
});

test("pip 缺少 trusted-host 时只提示补 [install] 节", () => {
  const rows = devEnvRows(report({ pipMirror: mirror(PIP, { state: "untrusted" }) }));
  assert.equal(rows[1]?.value, "缺少 trusted-host");
  assert.equal(rows[1]?.hint?.code, "[install]\ntrusted-host = mirrors.aliyun.com");
  // 已有 [install] 节时应追加到原 trusted-host，而不是再写一个同名节（pip 读到重复节会整体报错）。
  assert.ok(rows[1]?.hint?.text.includes("追加到其中的 trusted-host"));
  assert.ok(rows[1]?.hint?.text.includes("不要再新增一个 [install] 节"));
});

test("npm 未启用给出 npm config set 命令，检测失败展示原因", () => {
  const off = devEnvRows(report({ npmMirror: mirror(NPM, { state: "notConfigured", current: "https://registry.npmjs.org/" }) }));
  assert.equal(off[3]?.addr, "https://registry.npmjs.org/");
  assert.equal(off[3]?.hint?.code, `npm config set registry ${NPM}`);

  const failed = devEnvRows(report({ npmMirror: mirror(NPM, { state: "failed", current: null, detail: "PATH 上找不到 npm" }) }));
  assert.deepEqual([failed[3]?.value, failed[3]?.addr, failed[3]?.hint?.text], ["检测失败", "—", "PATH 上找不到 npm"]);
});

test("pipIniSnippet 仅在 http 源时写 trusted-host", () => {
  assert.equal(
    pipIniSnippet(PIP),
    "[global]\nindex-url = http://mirrors.aliyun.com/pypi/simple/\n\n[install]\ntrusted-host = mirrors.aliyun.com",
  );
  assert.equal(pipIniSnippet("https://mirrors.aliyun.com/pypi/simple/"), "[global]\nindex-url = https://mirrors.aliyun.com/pypi/simple/");
});

test("hostOf / formatClock", () => {
  assert.equal(hostOf("http://mirrors.aliyun.com:8080/pypi/simple/"), "mirrors.aliyun.com");
  assert.equal(hostOf("not a url"), "not a url");
  assert.equal(formatClock(new Date(2026, 0, 2, 3, 4, 5).getTime()), "03:04:05");
});
