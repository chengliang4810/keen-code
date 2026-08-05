import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";
import process from "node:process";

/** Tauri 桌面开发地址；环境变量可覆盖默认回环地址。 */
const host = process.env.TAURI_DEV_HOST || "127.0.0.1";

export default defineConfig(() => ({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("/highlight.js/")) return "vendor-highlight";
          if (id.includes("/xlsx/") || id.includes("/docx-preview/")) return "vendor-office";
          if (id.includes("/plyr/")) return "vendor-media";
          if (id.includes("/@tiptap/") || id.includes("/tiptap-markdown/")) return "vendor-tiptap";
        },
      },
    },
  },
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    host,
    hmr: {
      protocol: "ws",
      host,
      port: 1422,
    },
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.{test,spec}.ts", "src/**/*.{test,spec}.tsx"],
  },
}));
