// 共用的 model_catalog_json 移除确认说明（设计 §8：打开开关前与 catalogConfirmationRequired 错误
// 两个触发点使用同一份说明文案）。
import { confirmModal } from "./modal";

export function catalogConfirmMessageForPath(path: string): string {
  return `config.toml 中存在 model_catalog_json 指针（${path}）。`;
}

/** `reasonLine` 是给用户看的第一行说明（通常来自后端错误信息或按路径拼出的提示）。 */
export function confirmCatalogRemoval(reasonLine: string): Promise<boolean> {
  return confirmModal({
    title: "移除 model_catalog_json 指针",
    message: [reasonLine, "继续将移除该指针，但不会删除外部文件。"],
    confirmText: "确认移除并继续",
    cancelText: "取消",
  });
}
