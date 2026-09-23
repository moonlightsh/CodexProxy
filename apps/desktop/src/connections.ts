// 纯函数：最近连接列表的截断逻辑（设计 §8：内存保留 50 条，最新在前）。不依赖 DOM。
import type { ConnectionRecord } from "./types";

export const MAX_RECENT_CONNECTIONS = 50;

/** 把一条新记录插入列表头部，并截断到最多 50 条。不修改传入的数组。 */
export function withNewConnection(
  list: readonly ConnectionRecord[],
  record: ConnectionRecord,
): ConnectionRecord[] {
  return [record, ...list].slice(0, MAX_RECENT_CONNECTIONS);
}

/** 把一个可能超长的列表截断到最多 50 条（用于初始加载）。不修改传入的数组。 */
export function truncateConnections(list: readonly ConnectionRecord[]): ConnectionRecord[] {
  return list.slice(0, MAX_RECENT_CONNECTIONS);
}
