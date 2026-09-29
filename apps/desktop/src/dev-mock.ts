// 开发预览专用 mock（任务 3.2 要求 10）：仅在 `npm run dev` 且不存在 Tauri 运行时
// （普通浏览器）时由 main.ts 动态加载，模拟 invoke 与事件推送，便于直接在浏览器里
// 预览首次运行、needsKey、残留、旧块、配置错误等各种状态。
//
// 生产构建不会打包本文件：main.ts 里对本模块的 import 包在
// `if (import.meta.env.DEV && ...)` 分支内，`import.meta.env.DEV` 在生产构建时
// 被 Vite 替换为编译期常量 `false`，整个分支（含动态 import）会被裁剪掉。
// 可用下面导出的 DEV_MOCK_MARKER 在构建后核对 dist 产物不包含本文件的痕迹。
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";

import type {
  ConnectionRecord,
  DevEnvReport,
  EnableRequest,
  ErrorPayload,
  ReconcileReport,
  SaveKeyRequest,
  Status,
} from "./types";
import { EVENT_CONNECTION_RECORDED, EVENT_STATUS_CHANGED } from "./types";

/** 仅用于验证生产构建未打包本文件的特征字符串。 */
export const DEV_MOCK_MARKER = "__CODEX_HELPER_DEV_MOCK__";

function baseStatus(): Status {
  return {
    platformSupported: true,
    enabled: false,
    keyConfigured: false,
    needsKey: false,
    proxy: { state: "stopped", port: 17891 },
    gateway: "unknown",
    socks5: "unknown",
    gatewayAddr: "10.20.30.61:8080",
    socks5Addr: "10.20.30.61:7891",
    configManaged: false,
    envManaged: false,
    residue: false,
    legacyEnvBlock: false,
    modelCatalogJson: null,
    configError: null,
    autostart: false,
    codexHome: "/dev/fake-codex-home",
  };
}

/**
 * 用 `?scenario=` 选择预览的状态，例如：
 * `?scenario=needsKey`、`?scenario=residue`、`?scenario=legacy`、
 * `?scenario=configError`、`?scenario=dev`、`?scenario=catalog`、`?scenario=running`（默认）。
 * Key 录入区里输入 `bad` 模拟 keyRejected，输入 `slow` 模拟 gatewayUnavailable。
 */
function applyScenario(status: Status, scenario: string): Status {
  switch (scenario) {
    case "firstRun":
      return status;
    case "needsKey":
      return { ...status, enabled: true, keyConfigured: false, needsKey: true };
    case "residue":
      return { ...status, residue: true };
    case "legacy":
      return { ...status, legacyEnvBlock: true };
    case "configError":
      return { ...status, configError: "config.toml 第 12 行解析失败：意外的表名。" };
    case "dev":
      return { ...status, platformSupported: false };
    case "catalog":
      return {
        ...status,
        enabled: true,
        keyConfigured: true,
        proxy: { state: "running", port: status.proxy.port },
        modelCatalogJson: "/dev/fake-codex-home/model_catalog.json",
      };
    case "running":
    default:
      return {
        ...status,
        enabled: true,
        keyConfigured: true,
        proxy: { state: "running", port: status.proxy.port },
        gateway: "reachable",
        socks5: "reachable",
        configManaged: true,
        envManaged: true,
      };
  }
}

/**
 * 开发环境页预览：`?devenv=` 选择检测结果，可与 `?scenario=` 组合。
 * 取值：ok（默认）、missing、old、stub、broken、nomirror、untrusted、failed、dev。
 */
function devEnvReport(kind: string): DevEnvReport {
  const pip = "http://mirrors.aliyun.com/pypi/simple/";
  const npm = "https://registry.npmmirror.com/";
  const report: DevEnvReport = {
    platformSupported: true,
    python: {
      state: "ok",
      path: "C:\\Users\\dev\\AppData\\Local\\Programs\\Python\\Python313\\python.exe",
      version: "3.13.14",
      minVersion: "3.13",
      detail: null,
    },
    pipMirror: { state: "configured", current: pip, expected: pip, detail: null },
    node: { state: "ok", path: "C:\\Tools\\nodejs\\node.exe", version: "24.16.0", minVersion: null, detail: null },
    npmMirror: { state: "configured", current: npm, expected: npm, detail: null },
  };
  const skippedPip = { state: "skipped" as const, current: null, expected: pip, detail: null };
  const skippedNpm = { state: "skipped" as const, current: null, expected: npm, detail: null };
  switch (kind) {
    case "missing":
      return {
        ...report,
        python: { ...report.python, state: "missing", path: null, version: null },
        pipMirror: skippedPip,
        node: { ...report.node, state: "missing", path: null, version: null },
        npmMirror: skippedNpm,
      };
    case "old":
      return { ...report, python: { ...report.python, state: "tooOld", version: "3.12.9" }, pipMirror: skippedPip };
    case "stub":
      return {
        ...report,
        python: {
          ...report.python,
          state: "storeStub",
          path: "C:\\Users\\dev\\AppData\\Local\\Microsoft\\WindowsApps\\python.exe",
          version: null,
          detail: "python --version 没有输出版本号（退出码 9009）",
        },
        pipMirror: skippedPip,
      };
    case "broken":
      return {
        ...report,
        node: { ...report.node, state: "broken", version: null, detail: "node --version 超过 15 秒未完成" },
        npmMirror: skippedNpm,
      };
    case "nomirror":
      return {
        ...report,
        pipMirror: { ...report.pipMirror, state: "notConfigured", current: null },
        npmMirror: { ...report.npmMirror, state: "notConfigured", current: "https://registry.npmjs.org/" },
      };
    case "untrusted":
      return { ...report, pipMirror: { ...report.pipMirror, state: "untrusted" } };
    case "failed":
      return {
        ...report,
        pipMirror: { ...report.pipMirror, state: "failed", current: null, detail: "当前 Python 没有安装 pip" },
        npmMirror: { ...report.npmMirror, state: "failed", current: null, detail: "PATH 上找不到 npm" },
      };
    case "dev":
      return { ...report, platformSupported: false };
    default:
      return report;
  }
}

function sampleConnections(): ConnectionRecord[] {
  return [
    { time: Date.now() - 4000, host: "chatgpt.com", port: 443, decision: "socks5", ok: true, ms: 120 },
    { time: Date.now() - 9000, host: "github.com", port: 443, decision: "direct", ok: true, ms: 40 },
    { time: Date.now() - 15000, host: "api.openai.com", port: 443, decision: "socks5", ok: false, ms: 3005 },
  ];
}

export function installDevMock(): void {
  const params = new URLSearchParams(window.location.search);
  const scenario = params.get("scenario") ?? "running";
  const devEnvKind = params.get("devenv") ?? "ok";

  let status = applyScenario(baseStatus(), scenario);
  let connections = sampleConnections();
  const reconcileReport: ReconcileReport | null =
    scenario === "reconcileFailed"
      ? {
          outcome: "failed",
          error: { code: "configInvalid", message: "启动对账失败（示例原因，仅预览用）。" },
          status,
        }
      : null;

  const setStatus = (next: Status) => {
    status = next;
    void emit(EVENT_STATUS_CHANGED, status);
  };

  const reject = (payload: ErrorPayload): never => {
    throw payload;
  };

  mockIPC(
    (cmd, rawArgs) => {
      const args = (rawArgs ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "get_status":
        case "refresh_status":
          return status;

        case "enable": {
          const request = args.request as EnableRequest;
          if (request.key !== undefined) {
            const trimmed = request.key.trim();
            if (!trimmed) return reject({ code: "emptyKey", message: "Key 不能为空。" });
            if (trimmed === "bad") {
              return reject({ code: "keyRejected", message: "网关拒绝了该 Key（401）。" });
            }
            if (trimmed === "slow" && !request.forceSave) {
              return reject({ code: "gatewayUnavailable", message: "网关暂时不可用（超时，仅预览用）。" });
            }
          } else if (!status.keyConfigured) {
            return reject({ code: "credentialMissing", message: "未提供 Key 且尚未保存过凭据。" });
          }
          if (status.modelCatalogJson && !request.confirmRemoveCatalog) {
            return reject({
              code: "catalogConfirmationRequired",
              message: `config.toml 中存在 model_catalog_json 指针（${status.modelCatalogJson}）。`,
            });
          }
          setStatus({
            ...status,
            enabled: true,
            keyConfigured: true,
            needsKey: false,
            modelCatalogJson: request.confirmRemoveCatalog ? null : status.modelCatalogJson,
            proxy: { state: "running", port: status.proxy.port },
            gateway: "reachable",
            socks5: "reachable",
            configManaged: true,
            envManaged: true,
          });
          return status;
        }

        case "disable": {
          setStatus({
            ...status,
            enabled: false,
            needsKey: false,
            proxy: { state: "stopped", port: status.proxy.port },
            configManaged: false,
            envManaged: false,
          });
          return status;
        }

        case "save_key": {
          const request = args.request as SaveKeyRequest;
          const trimmed = request.key.trim();
          if (!trimmed) return reject({ code: "emptyKey", message: "Key 不能为空。" });
          if (trimmed === "bad") {
            return reject({ code: "keyRejected", message: "网关拒绝了该 Key（401）。" });
          }
          if (trimmed === "slow" && !request.forceSave) {
            return reject({ code: "gatewayUnavailable", message: "网关暂时不可用（超时，仅预览用）。" });
          }
          setStatus({ ...status, keyConfigured: true, needsKey: false });
          return status;
        }

        case "clear_key": {
          setStatus({ ...status, keyConfigured: false, needsKey: status.enabled });
          return status;
        }

        case "cleanup_residue": {
          setStatus({ ...status, residue: false });
          return status;
        }

        case "remove_legacy_env_block": {
          setStatus({ ...status, legacyEnvBlock: false });
          return status;
        }

        case "set_autostart": {
          setStatus({ ...status, autostart: Boolean(args.enabled) });
          return status;
        }

        case "check_update":
          // 预览可用更新：`?scenario=updateAvailable`；其余场景返回“已是最新”。
          return scenario === "updateAvailable"
            ? {
                available: true,
                currentVersion: "0.1.1",
                version: "0.2.0",
                notes: "示例更新说明（仅预览用）。",
                date: "2026-09-26T00:00:00Z",
              }
            : { available: false, currentVersion: "0.1.1", version: null, notes: null, date: null };

        case "install_update":
          // 忽略 expectedVersion 参数；开发预览不支持真实安装。
          return reject({ code: "update", message: "开发预览不支持真实安装更新。" });

        case "app_version":
          return "0.1.1";

        case "detect_dev_env":
          // 模拟检测耗时，便于预览“检测中…”状态。
          return new Promise((resolve) => {
            window.setTimeout(() => resolve(devEnvReport(devEnvKind)), 800);
          });

        case "recent_connections":
          return connections;

        case "get_reconcile_report":
          return reconcileReport;

        default:
          return reject({ code: "internal", message: `开发预览未实现的命令：${String(cmd)}` });
      }
    },
    { shouldMockEvents: true },
  );

  // 定期追加一条连接记录，直观预览最近连接区域的截断效果。
  window.setInterval(() => {
    const hosts = ["chatgpt.com", "api.openai.com", "example.com", "github.com"];
    const host = hosts[Math.floor(Math.random() * hosts.length)] ?? "example.com";
    const record: ConnectionRecord = {
      time: Date.now(),
      host,
      port: 443,
      decision: host === "example.com" ? "direct" : "socks5",
      ok: Math.random() > 0.15,
      ms: Math.round(20 + Math.random() * 300),
    };
    connections = [record, ...connections].slice(0, 50);
    void emit(EVENT_CONNECTION_RECORDED, record);
  }, 5000);
}
