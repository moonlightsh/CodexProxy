import type { DownloadProgress } from "./types";

export type ComponentDownloadState =
  | { kind: "downloading"; id: string; receivedBytes: number; totalBytes: number | null; started: boolean; cancelling: boolean }
  | { kind: "completed"; version: string; path: string }
  | { kind: "failed"; message: string }
  | { kind: "cancelled" };

function bytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
}

export function downloadView(state: ComponentDownloadState | undefined): {
  text: string; path: string | null; percent: number | null; canCancel: boolean;
} {
  const base = { path: null, percent: null, canCancel: false };
  if (!state) return { ...base, text: "" };
  switch (state.kind) {
    case "completed": return { ...base, text: `v${state.version} 下载完成，请自行安装。`, path: state.path };
    case "failed": return { ...base, text: `下载失败：${state.message}` };
    case "cancelled": return { ...base, text: "下载已取消，可重新下载。" };
    case "downloading": {
      const size = state.totalBytes !== null && state.totalBytes > 0 ? state.totalBytes : null;
      return {
        ...base,
        text: state.cancelling ? "正在取消…" : !state.started ? "准备下载…" :
          `经代理下载：${bytes(state.receivedBytes)}${size !== null ? ` / ${bytes(size)}` : "（总大小未知）"}`,
        percent: size !== null ? Math.min(100, state.receivedBytes / size * 100) : null,
        canCancel: state.started && !state.cancelling,
      };
    }
  }
}

export function updateDownloadProgress(
  state: ComponentDownloadState | undefined, id: string, progress: DownloadProgress,
): ComponentDownloadState | undefined {
  if (state?.kind !== "downloading" || state.id !== id) return state;
  return { ...state, ...progress, started: true };
}
