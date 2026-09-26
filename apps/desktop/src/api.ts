// Tauri 命令封装（与 apps/desktop/src-tauri/src/commands.rs 一一对应）。
import { invoke } from "@tauri-apps/api/core";
import type {
  ConnectionRecord,
  EnableRequest,
  ErrorPayload,
  ReconcileReport,
  SaveKeyRequest,
  Status,
  UpdateCheck,
} from "./types";

export const api = {
  getStatus: () => invoke<Status>("get_status"),
  refreshStatus: () => invoke<Status>("refresh_status"),
  enable: (request: EnableRequest) => invoke<Status>("enable", { request }),
  disable: () => invoke<Status>("disable"),
  saveKey: (request: SaveKeyRequest) => invoke<Status>("save_key", { request }),
  clearKey: () => invoke<Status>("clear_key"),
  cleanupResidue: () => invoke<Status>("cleanup_residue"),
  removeLegacyEnvBlock: () => invoke<Status>("remove_legacy_env_block"),
  setAutostart: (enabled: boolean) => invoke<Status>("set_autostart", { enabled }),
  recentConnections: () => invoke<ConnectionRecord[]>("recent_connections"),
  getReconcileReport: () => invoke<ReconcileReport | null>("get_reconcile_report"),
  /** 检测是否有可用更新（只检测、不下载）。 */
  checkUpdate: () => invoke<UpdateCheck>("check_update"),
  /** 下载并安装可用更新，成功后应用自动重启（此 Promise 在成功时不会 resolve）。 */
  installUpdate: () => invoke<void>("install_update"),
};

/** 把 invoke 抛出的错误规范化为 ErrorPayload。 */
export function toErrorPayload(error: unknown): ErrorPayload {
  if (error && typeof error === "object" && "code" in error && "message" in error) {
    return error as ErrorPayload;
  }
  return { code: "internal", message: String(error) };
}
