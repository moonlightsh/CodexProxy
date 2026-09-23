/// <reference types="vite/client" />

// 仅在非 Tauri 运行时（普通浏览器预览）存在；用于判断是否加载开发期 mock（任务 3.2 要求 10）。
interface Window {
  __TAURI_INTERNALS__?: unknown;
}
