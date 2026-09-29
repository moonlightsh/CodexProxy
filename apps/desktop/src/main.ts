// 阶段 3 任务 3.2 实现：按设计 §8 的界面；§16 起分为“受管网关”“开发环境”两个标签页。
// 本文件只负责启动（挂载各区域、发起初始数据加载、注册 Tauri 事件监听）与
// 各区域回调之间的编排；纯逻辑（格式化、错误码映射、连接列表截断）见对应的
// 无 DOM 依赖模块，并有各自的单元测试。
import { listen } from "@tauri-apps/api/event";

import { api, toErrorPayload } from "./api";
import { createBannerRegion } from "./banners";
import { catalogConfirmMessageForPath, confirmCatalogRemoval } from "./catalogconfirm";
import { withNewConnection, truncateConnections } from "./connections";
import { createConnectionsListRegion } from "./connectionslist";
import { createDevEnvPanel } from "./devenvpanel";
import { el } from "./dom";
import { classifyKeyError } from "./errors";
import { createFooterRegion } from "./footer";
import { isFirstRun, keySubmitAction } from "./format";
import { createKeyPanel } from "./keypanel";
import { createMessageRegion } from "./message";
import { confirmModal } from "./modal";
import { createStatusLightsRegion } from "./statuslights";
import { createSwitchRegion } from "./switchcontrol";
import { createTabs } from "./tabs";
import { createUpdatePanel } from "./updatepanel";
import type {
  ConnectionRecord,
  DevEnvReport,
  ErrorPayload,
  ReconcileReport,
  Status,
  UpdateCheck,
} from "./types";
import {
  EVENT_CONNECTION_RECORDED,
  EVENT_KEY_REQUIRED,
  EVENT_OPERATION_FAILED,
  EVENT_RECONCILE_FINISHED,
  EVENT_STATUS_CHANGED,
} from "./types";

/** 正在进行中的操作标识，用于禁用对应按钮并显示进行中文案（任务 3.2 要求 6）。 */
type OpKind =
  | "enable"
  | "disable"
  | "saveKey"
  | "clearKey"
  | "cleanupResidue"
  | "removeLegacyEnvBlock"
  | "setAutostart"
  | "checkUpdate"
  | "installUpdate"
  | "detectDevEnv";

async function main(): Promise<void> {
  // 开发预览：仅在 DEV 构建且不存在 Tauri 运行时注入时加载；`import.meta.env.DEV`
  // 是编译期常量，生产构建会连同这个分支与动态 import 一起被裁剪（任务 3.2 要求 10）。
  if (import.meta.env.DEV && typeof window.__TAURI_INTERNALS__ === "undefined") {
    const devMock = await import("./dev-mock");
    devMock.installDevMock();
  }

  const appRoot = document.querySelector<HTMLDivElement>("#app");
  if (!appRoot) return;

  let status: Status | null = null;
  let connections: ConnectionRecord[] = [];
  // 升级渠道：当前版本与最近一次检测到的可用更新（升级检测/安装逻辑在 Rust 侧）。
  let updateCurrentVersion: string | null = null;
  let updateAvailable: UpdateCheck | null = null;
  // 开发环境页（设计 §16）：最近一次检测结果与完成时间。
  let devEnvReport: DevEnvReport | null = null;
  let devEnvCheckedAt: number | null = null;
  const busy = new Set<OpKind>();

  const messageRegion = createMessageRegion();

  const bannerRegion = createBannerRegion({
    onCleanupResidue: () => {
      void runSimpleOp("cleanupResidue", () => api.cleanupResidue(), "已清理残留配置，修改后请重启 Codex");
    },
    onRemoveLegacyEnvBlock: () => {
      void runSimpleOp(
        "removeLegacyEnvBlock",
        () => api.removeLegacyEnvBlock(),
        "已移除旧的受管块，修改后请重启 Codex",
      );
    },
  });

  const keyPanel = createKeyPanel({
    onSubmit: (key) => {
      if (!status) return;
      void attemptKeyOperation(keySubmitAction(status), key, false, false);
    },
    onRequestClear: () => {
      void handleRequestClear();
    },
  });

  const switchRegion = createSwitchRegion({
    onTurnOn: () => {
      void handleTurnOn();
    },
    onTurnOff: () => {
      void runSimpleOp("disable", () => api.disable(), "已停用受管模式，修改后请重启 Codex");
    },
  });

  const statusLightsRegion = createStatusLightsRegion();
  const connectionsListRegion = createConnectionsListRegion();

  const footerRegion = createFooterRegion({
    onToggleAutostart: (enabled) => {
      void runSimpleOp("setAutostart", () => api.setAutostart(enabled), "已更新开机自启设置");
    },
  });

  const updatePanel = createUpdatePanel({
    onCheck: () => {
      void runCheckUpdate(true);
    },
    onInstall: () => {
      void runInstallUpdate();
    },
  });

  const devEnvPanel = createDevEnvPanel({
    onDetect: () => {
      void runDetectDevEnv();
    },
  });

  const title = el("h1", { class: "app-title" }, ["CodexHelper"]);
  const loading = el("p", { class: "app-loading" }, ["加载中…"]);
  const connectionsTitle = el("h2", { class: "section-title" }, ["最近连接（最多 50 条）"]);
  // 首次运行（未配置 Key 且未启用）时整体隐藏：只留横幅与 Key 录入区（任务 3.2 要求 2）。
  // 受管模式开关单独隐藏（首次运行时还没有可切换的对象），状态灯/连接列表/底部一起隐藏。
  const normalModeRegion = el("div", { class: "normal-mode" }, [
    statusLightsRegion.root,
    connectionsTitle,
    connectionsListRegion.root,
    footerRegion.root,
  ]);
  // 布局自上而下：横幅 → 受管模式开关 → API Key → 状态灯/连接/底部（任务 3.2 要求 1）。
  const content = el("div", { class: "app-content" }, [
    bannerRegion.root,
    switchRegion.root,
    keyPanel.root,
    normalModeRegion,
  ]);
  content.hidden = true;

  // 两个页面：受管网关（原单页）与开发环境（设计 §16）。更新区放在页面之外，两页都可见
  // （升级与是否配置 Key 无关，首次运行也可见）。
  const gatewayPage = el("div", { class: "page" }, [loading, content]);
  const devEnvPage = el("div", { class: "page" }, [devEnvPanel.root]);
  const tabs = createTabs(
    [
      { id: "gateway", label: "受管网关", panel: gatewayPage },
      { id: "devenv", label: "开发环境", panel: devEnvPage },
    ],
    (page) => {
      // 首次打开开发环境页时自动检测一次；启动时不检测，避免开机即拉起子进程。
      if (page === "devenv" && devEnvReport === null) void runDetectDevEnv();
    },
  );
  tabs.select("gateway");

  appRoot.append(title, tabs.root, messageRegion.root, gatewayPage, devEnvPage, updatePanel.root);

  function render(): void {
    renderGateway();
    devEnvPanel.update({
      detecting: busy.has("detectDevEnv"),
      report: devEnvReport,
      checkedAt: devEnvCheckedAt,
    });
    updatePanel.update({
      currentVersion: updateCurrentVersion,
      checking: busy.has("checkUpdate"),
      installing: busy.has("installUpdate"),
      available: updateAvailable,
    });
  }

  function renderGateway(): void {
    if (!status) {
      loading.hidden = false;
      content.hidden = true;
      return;
    }
    loading.hidden = true;
    content.hidden = false;

    bannerRegion.update(status, busy);
    keyPanel.update(status, busy);

    const firstRun = isFirstRun(status);
    switchRegion.root.hidden = firstRun;
    normalModeRegion.hidden = firstRun;
    switchRegion.update(status, { enabling: busy.has("enable"), disabling: busy.has("disable") });
    statusLightsRegion.update(status);
    connectionsListRegion.update(connections);
    footerRegion.update(status, busy.has("setAutostart"));
  }

  /** 开发环境检测：只读，失败（invoke 本身出错）时在消息区提示，保留上一次的结果。 */
  async function runDetectDevEnv(): Promise<void> {
    if (busy.has("detectDevEnv")) return;
    busy.add("detectDevEnv");
    render();
    try {
      devEnvReport = await api.detectDevEnv();
      devEnvCheckedAt = Date.now();
    } catch (error) {
      messageRegion.show(toErrorPayload(error).message, "error");
    } finally {
      busy.delete("detectDevEnv");
      render();
    }
  }

  /** enable / saveKey 的通用重试编排：Key 只作为函数参数在调用链中传递，不落入任何模块状态。 */
  async function attemptKeyOperation(
    kind: "enable" | "saveKey",
    key: string,
    forceSave: boolean,
    confirmRemoveCatalog: boolean,
  ): Promise<void> {
    busy.add(kind);
    // forceSave 为 true 表示这是用户点击“仍然保存”发起的重试：进行中禁用该按钮并
    // 显示“保存中…”，避免用户双击造成并发的多次 forceSave 请求。
    if (forceSave) keyPanel.setGatewayRetrying(true);
    render();
    try {
      const next =
        kind === "enable"
          ? await api.enable({
              key,
              forceSave: forceSave || undefined,
              confirmRemoveCatalog: confirmRemoveCatalog || undefined,
            })
          : await api.saveKey({ key, forceSave: forceSave || undefined });
      status = next;
      keyPanel.resetFeedback();
      keyPanel.closeForm();
      messageRegion.show(
        kind === "enable" ? "已启用受管模式，修改后请重启 Codex" : "Key 已保存，修改后请重启 Codex",
        "success",
      );
    } catch (error) {
      await handleKeyError(kind, key, forceSave, confirmRemoveCatalog, toErrorPayload(error));
    } finally {
      busy.delete(kind);
      // 重试解除“保存中…”状态；若 handleKeyError 已经用新内容替换了反馈区（例如再次
      // 展示 gatewayUnavailable），这里是安全的空操作（setGatewayRetrying 内部会判断
      // 当前反馈区是否还持有“仍然保存”按钮的引用）。
      if (forceSave) keyPanel.setGatewayRetrying(false);
      render();
    }
  }

  async function handleKeyError(
    kind: "enable" | "saveKey",
    key: string,
    forceSave: boolean,
    confirmRemoveCatalog: boolean,
    payload: ErrorPayload,
  ): Promise<void> {
    // forceSave 重试无论以何种方式收场，都要先清掉旧的“仍然保存”反馈（连同其闭包中的
    // Key 引用一并丢弃），避免残留一个仍可点击、仍握着暂存 Key 的按钮。下面按错误类型
    // 需要时会立刻用新内容重新填充反馈区。
    if (forceSave) keyPanel.resetFeedback();
    switch (classifyKeyError(payload)) {
      case "inline":
        keyPanel.showInlineError(payload.message);
        return;
      case "gatewayUnavailable":
        keyPanel.showGatewayUnavailable(
          payload.message,
          () => {
            void attemptKeyOperation(kind, key, true, confirmRemoveCatalog);
          },
          () => {
            // 用户主动取消：丢弃暂存的 Key（不再持有可触发重试的按钮）。
            keyPanel.resetFeedback();
          },
        );
        return;
      case "catalogConfirmation": {
        const path = status?.modelCatalogJson ?? "";
        const ok = await confirmCatalogRemoval(payload.message || catalogConfirmMessageForPath(path));
        if (ok) {
          await attemptKeyOperation(kind, key, forceSave, true);
        }
        return;
      }
      case "message":
      default:
        messageRegion.show(payload.message, "error");
    }
  }

  async function handleTurnOn(): Promise<void> {
    if (!status) return;
    if (status.modelCatalogJson) {
      const ok = await confirmCatalogRemoval(catalogConfirmMessageForPath(status.modelCatalogJson));
      if (!ok) return;
      await runSwitchEnable(true);
    } else {
      await runSwitchEnable(false);
    }
  }

  async function runSwitchEnable(confirmRemoveCatalog: boolean): Promise<void> {
    busy.add("enable");
    render();
    try {
      const next = await api.enable({ confirmRemoveCatalog: confirmRemoveCatalog || undefined });
      status = next;
      messageRegion.show("已启用受管模式，修改后请重启 Codex", "success");
    } catch (error) {
      const payload = toErrorPayload(error);
      if (classifyKeyError(payload) === "catalogConfirmation") {
        const ok = await confirmCatalogRemoval(payload.message);
        if (ok) {
          await runSwitchEnable(true);
        }
        // 用户主动取消：不当作错误展示。
        return;
      }
      messageRegion.show(payload.message, "error");
    } finally {
      busy.delete("enable");
      render();
    }
  }

  async function handleRequestClear(): Promise<void> {
    const ok = await confirmModal({
      title: "清除 API Key",
      message: [
        "清除后本工具不再保存该 Key。",
        "若受管模式当前已启用，清除后 Codex 请求会失败，直到重新录入 Key。",
      ],
      confirmText: "清除 Key",
      cancelText: "取消",
      danger: true,
    });
    if (!ok) return;
    await runSimpleOp("clearKey", () => api.clearKey(), "已清除 Key");
  }

  async function runSimpleOp(kind: OpKind, task: () => Promise<Status>, successMessage: string): Promise<void> {
    busy.add(kind);
    render();
    try {
      status = await task();
      messageRegion.show(successMessage, "success");
    } catch (error) {
      messageRegion.show(toErrorPayload(error).message, "error");
    } finally {
      busy.delete(kind);
      render();
    }
  }

  /** 检查更新：manual=true 为用户点击（无更新时提示“已是最新”），false 为启动时静默自检。 */
  async function runCheckUpdate(manual: boolean): Promise<void> {
    if (busy.has("checkUpdate") || busy.has("installUpdate")) return;
    busy.add("checkUpdate");
    render();
    try {
      const result = await api.checkUpdate();
      updateCurrentVersion = result.currentVersion;
      updateAvailable = result.available ? result : null;
      if (result.available) {
        messageRegion.show(`发现新版本 v${result.version}，可在下方“立即更新”安装。`, "info");
      } else if (manual) {
        messageRegion.show(`已是最新版本 v${result.currentVersion}。`, "success");
      }
    } catch (error) {
      // 启动静默自检失败不打扰用户（例如内网升级服务器暂不可达）；仅手动检查时提示。
      if (manual) messageRegion.show(toErrorPayload(error).message, "error");
    } finally {
      busy.delete("checkUpdate");
      render();
    }
  }

  /** 下载并安装更新：用户确认后执行，成功后应用自动重启（invoke 在成功时不会 resolve）。 */
  async function runInstallUpdate(): Promise<void> {
    if (busy.has("installUpdate") || busy.has("checkUpdate")) return;
    const target = updateAvailable;
    if (!target?.version) return;
    const expectedVersion = target.version;
    // 先占用 installUpdate 并 render：确认框弹出期间“检查更新/立即更新”按钮均禁用，避免并发。
    busy.add("installUpdate");
    render();
    const message = ["确认下载并安装更新吗？安装完成后应用会自动重启。"];
    const notes = target.notes?.trim();
    if (notes) message.push(notes);
    const ok = await confirmModal({
      title: `更新到 v${expectedVersion}`,
      message,
      confirmText: "下载并安装",
      cancelText: "取消",
    });
    if (!ok) {
      busy.delete("installUpdate");
      render();
      return;
    }
    try {
      // 把用户确认的版本传给后端；若服务器版本已变，后端会拒绝安装。
      await api.installUpdate(expectedVersion);
      // 成功时应用已开始重启，这里通常不会执行到；万一 resolve 也不做额外处理。
    } catch (error) {
      busy.delete("installUpdate");
      render();
      messageRegion.show(toErrorPayload(error).message, "error");
      // 版本可能已在服务器变化：重新静默检查，刷新更新区展示与可安装版本。
      void runCheckUpdate(false);
    }
  }

  /** reconcile-finished 事件（实时）：对账是当前正在发生的操作，其 status 就是最新状态。 */
  function applyReconcileReport(report: ReconcileReport): void {
    status = report.status;
    showReconcileFailure(report);
    render();
  }

  /**
   * 启动时补齐 get_reconcile_report：这份报告是启动对账当时存下的快照，一定不比
   * getStatus() 的结果新（没有桌面层随后做的可达性探测，也没有用户在此之间的操作），
   * 所以这里只用它展示 outcome=failed 的错误信息，状态一律以 getStatus 为准，
   * 避免把界面回退到过时的启动快照（评审发现的阻断项 2）。
   */
  function showReconcileFailure(report: ReconcileReport): void {
    if (report.outcome === "failed" && report.error) {
      messageRegion.show(report.error.message, "error");
    }
  }

  // 先注册事件监听，再补齐初始状态：事件可能早于监听注册触发（任务 3.2 要求 5）。
  await Promise.all([
    listen<Status>(EVENT_STATUS_CHANGED, (event) => {
      status = event.payload;
      render();
    }),
    listen<ConnectionRecord>(EVENT_CONNECTION_RECORDED, (event) => {
      // 只更新连接列表，不做整页 render()：banners 等区域会在每次 render() 时重建其
      // DOM（包括按钮），高频的连接事件会打断用户对横幅按钮的操作（评审发现的 minor 项）。
      connections = withNewConnection(connections, event.payload);
      connectionsListRegion.update(connections);
    }),
    listen<null>(EVENT_KEY_REQUIRED, () => {
      // Key 录入区在受管网关页；用户停在开发环境页时先切回，否则录入框不可见、无法聚焦。
      tabs.select("gateway");
      keyPanel.openForm();
      keyPanel.focusInput();
    }),
    listen<ReconcileReport>(EVENT_RECONCILE_FINISHED, (event) => {
      applyReconcileReport(event.payload);
    }),
    listen<ErrorPayload>(EVENT_OPERATION_FAILED, (event) => {
      messageRegion.show(event.payload.message, "error");
    }),
  ]);

  // 当前版本来自本地（不依赖升级服务器可达）：独立获取，确保更新区始终显示版本。
  try {
    updateCurrentVersion = await api.appVersion();
    render();
  } catch {
    // 极少失败；此时更新区显示“当前版本 —”，不打扰用户。
    updateCurrentVersion = null;
  }

  try {
    const [initialStatus, initialConnections, reconcileReport] = await Promise.all([
      api.getStatus(),
      api.recentConnections(),
      api.getReconcileReport(),
    ]);
    status = initialStatus;
    connections = truncateConnections(initialConnections);
    if (reconcileReport) {
      showReconcileFailure(reconcileReport);
    }
    render();
    if (status.needsKey) {
      tabs.select("gateway");
      keyPanel.openForm();
      keyPanel.focusInput();
    }
  } catch (error) {
    messageRegion.show(toErrorPayload(error).message, "error");
  }

  // 启动时静默自检更新（不阻塞初始加载，失败不打扰）；发现新版本时在更新区提示，由用户确认安装。
  void runCheckUpdate(false);
}

void main();
