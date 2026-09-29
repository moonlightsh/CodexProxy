// 与 crates/helper-core/src/types.rs 一一对应（serde camelCase）。修改任何一侧都必须同步另一侧。

export type Route = "socks5" | "direct";
export type KeyCheck = "ok" | "unauthorized" | "serverError" | "timeoutOrNetwork";
export type Reachability = "unknown" | "reachable" | "unreachable";
export type ProxyState = "stopped" | "running" | "external";

export interface ProxyStatus {
  state: ProxyState;
  port: number;
}

export interface ConnectionRecord {
  /** 连接开始时间，Unix 毫秒 */
  time: number;
  host: string;
  port: number;
  decision: Route;
  ok: boolean;
  ms: number;
}

export interface Status {
  platformSupported: boolean;
  enabled: boolean;
  keyConfigured: boolean;
  needsKey: boolean;
  proxy: ProxyStatus;
  gateway: Reachability;
  socks5: Reachability;
  gatewayAddr: string;
  socks5Addr: string;
  configManaged: boolean;
  envManaged: boolean;
  residue: boolean;
  legacyEnvBlock: boolean;
  modelCatalogJson: string | null;
  configError: string | null;
  autostart: boolean;
  codexHome: string;
}

export interface EnableRequest {
  /** 新录入的 Key；省略表示使用已保存的凭据 */
  key?: string;
  forceSave?: boolean;
  confirmRemoveCatalog?: boolean;
}

export interface SaveKeyRequest {
  key: string;
  forceSave?: boolean;
}

export type ErrorCode =
  | "emptyKey"
  | "keyRejected"
  | "gatewayUnavailable"
  | "credentialMissing"
  | "catalogConfirmationRequired"
  | "portUnavailable"
  | "configInvalid"
  | "credential"
  | "io"
  | "update"
  | "internal";

export interface ErrorPayload {
  code: ErrorCode;
  message: string;
}

export type ReconcileOutcome = "idle" | "residueFound" | "applied" | "needsKey" | "failed";

export interface ReconcileReport {
  outcome: ReconcileOutcome;
  error: ErrorPayload | null;
  status: Status;
}

/** 内部升级渠道的检测结果（与 apps/desktop/src-tauri/src/update.rs 的 UpdateCheck 对应）。 */
export interface UpdateCheck {
  /** 是否有可用更新 */
  available: boolean;
  /** 当前运行版本 */
  currentVersion: string;
  /** 可用的新版本号（available 为 true 时存在） */
  version: string | null;
  /** 更新说明（manifest 的 notes 字段，可能为空） */
  notes: string | null;
  /** 发布日期（RFC3339 文本，可能为空） */
  date: string | null;
}

// ---- 开发环境检测（设计 §16），与 crates/helper-core/src/devenv.rs 一一对应 ----

export type RuntimeState = "missing" | "storeStub" | "broken" | "tooOld" | "ok";

export interface RuntimeReport {
  state: RuntimeState;
  /** 命令行实际会用到的可执行文件（PATH 上第一个命中） */
  path: string | null;
  /** 解析出的版本号，如 3.13.14 */
  version: string | null;
  /** 最低版本要求，如 3.13；无要求为 null */
  minVersion: string | null;
  /** state 为 broken / storeStub 时的原因 */
  detail: string | null;
}

export type MirrorState = "skipped" | "configured" | "untrusted" | "notConfigured" | "failed";

export interface MirrorReport {
  state: MirrorState;
  /** 当前生效的地址（已去掉 userinfo）；pip 使用默认源时为 null */
  current: string | null;
  /** 期望的地址 */
  expected: string;
  /** state 为 failed 时的原因 */
  detail: string | null;
}

export interface DevEnvReport {
  platformSupported: boolean;
  python: RuntimeReport;
  pipMirror: MirrorReport;
  node: RuntimeReport;
  npmMirror: MirrorReport;
}

/** Rust → 前端事件名 */
export const EVENT_STATUS_CHANGED = "status-changed";
export const EVENT_CONNECTION_RECORDED = "connection-recorded";
export const EVENT_KEY_REQUIRED = "key-required";
export const EVENT_RECONCILE_FINISHED = "reconcile-finished";
export const EVENT_OPERATION_FAILED = "operation-failed";
