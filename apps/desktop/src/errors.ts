// 纯函数：错误码到交互方式的映射（设计 §8、任务 3.2 要求 3）。不依赖 DOM。
import type { ErrorCode, ErrorPayload } from "./types";

/**
 * - inline：就地在输入框旁提示（emptyKey / keyRejected）
 * - gatewayUnavailable：显示原因，并提供“仍然保存”按钮
 * - catalogConfirmation：需要弹出确认移除 model_catalog_json 的模态框
 * - message：在消息区展示 `message`，无特殊交互
 */
export type KeyErrorKind = "inline" | "gatewayUnavailable" | "catalogConfirmation" | "message";

const INLINE_CODES: ReadonlySet<ErrorCode> = new Set(["emptyKey", "keyRejected"]);

export function classifyKeyError(payload: ErrorPayload): KeyErrorKind {
  const code = payload.code as ErrorCode;
  if (INLINE_CODES.has(code)) return "inline";
  if (code === "gatewayUnavailable") return "gatewayUnavailable";
  if (code === "catalogConfirmationRequired") return "catalogConfirmation";
  return "message";
}
