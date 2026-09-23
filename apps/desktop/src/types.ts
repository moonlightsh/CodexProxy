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
  | "internal";

export interface ErrorPayload {
  code: ErrorCode;
  message: string;
}

/** Rust → 前端事件名 */
export const EVENT_STATUS_CHANGED = "status-changed";
export const EVENT_CONNECTION_RECORDED = "connection-recorded";
export const EVENT_KEY_REQUIRED = "key-required";
