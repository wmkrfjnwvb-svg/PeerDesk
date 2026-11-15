import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 开发时把 /api 转发到本机运行的 peerdesk-server。
export default defineConfig({
  plugins: [react()],
  build: { outDir: "dist", sourcemap: false, assetsInlineLimit: 0 },
  server: { proxy: { "/api": "http://127.0.0.1:8080" } },
});
