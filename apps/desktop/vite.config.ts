import { defineConfig } from "vite";

// Tauri 开发服务器固定端口；与 tauri.conf.json 的 devUrl 保持一致。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
  },
});
