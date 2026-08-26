import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Build output location.
//
// `ASTOCK_BUILD_ROOT` relocates intermediates on a Windows workstation, where
// product policy keeps them off `C:`. It must not be *defaulted* to a Windows
// drive: on Linux and macOS `D:/astock-build/...` is not an absolute path, so
// Vite created a literal directory named `D:` inside `ui/` and `ui/dist` never
// existed — which silently broke the Tauri `frontendDist` lookup. The Cargo
// configuration had the same defect and was already fixed the same way.
//
// Default to `ui/dist`, which is what tauri.conf.json points at, and honour the
// override only when it is genuinely set.
const explicitRoot = process.env.ASTOCK_BUILD_ROOT?.trim();
const buildRoot = explicitRoot ? explicitRoot.replace(/\\/g, "/") : null;
const outDir = buildRoot ? `${buildRoot}/renderer-dist` : path.resolve(import.meta.dirname, "dist");
const cacheDir = buildRoot ? `${buildRoot}/vite-cache` : undefined;

export default defineConfig({
  plugins: [react()],
  ...(cacheDir ? { cacheDir } : {}),
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/.astock-renderer-dist/**"],
    },
  },
  build: {
    outDir,
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
