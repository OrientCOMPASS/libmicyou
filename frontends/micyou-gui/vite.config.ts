/*
 * libmicyou — micyou-gui frontend (original MicYou Vue UI on the libmicyou
 * backend). Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou vite configuration)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou adapter shim)
 *
 * GPL-3.0-or-later with the MicYou Plugin Exception. See LICENSE (repo root).
 */

import { defineConfig, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";
import path from "node:path";
import packageJson from "./package.json";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

/**
 * Redirect `@tauri-apps/api/core` imports made by the ported Vue sources to
 * the local compatibility shim (src/adapter/tauri-core.ts), which maps the
 * original MicYou Tauri commands onto the libmicyou `backend_rpc` contract.
 *
 * Implemented as a resolveId hook (not a resolve.alias entry) so the shim
 * itself — and anything under node_modules — still resolves the real module,
 * avoiding an infinite self-reference.
 */
function adapterShim(): Plugin {
  const adapterId = path
    .resolve(__dirname, "src/adapter/tauri-core.ts")
    .split(path.sep)
    .join("/");
  const adapterDir = path
    .resolve(__dirname, "src/adapter")
    .split(path.sep)
    .join("/");
  return {
    name: "micyou-adapter-shim",
    enforce: "pre",
    resolveId(source, importer) {
      if (source !== "@tauri-apps/api/core" || !importer) return null;
      const from = importer.split(path.sep).join("/");
      if (from.includes("node_modules")) return null;
      if (from.startsWith(adapterDir + "/")) return null;
      // Only application sources get the shim.
      if (!from.startsWith(path.resolve(__dirname).split(path.sep).join("/"))) {
        return null;
      }
      return adapterId;
    },
  };
}

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [adapterShim(), vue()],
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version),
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
