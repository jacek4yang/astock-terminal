import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const buildRoot = (process.env.ASTOCK_BUILD_ROOT || "D:/astock-build/astock-terminal").replace(/\\/g, "/");

export default defineConfig({
  plugins: [react()],
  cacheDir: `${buildRoot}/vite-cache`,
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/.astock-renderer-dist/**"],
    },
  },
  build: {
    outDir: `${buildRoot}/renderer-dist`,
    emptyOutDir: true,
    target: "es2022",
    chunkSizeWarningLimit: 1200,
    rollupOptions: {
      output: {
        manualChunks: {
          echarts: ["echarts"],
          vendor: ["react", "react-dom", "react-router-dom", "zustand"],
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    restoreMocks: true,
  },
});
