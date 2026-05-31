import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    react(),
    // Inline all JS/CSS into HTML to avoid ES module loading issues
    // in Flatpak's WebKitGTK (modules require CORS headers that
    // Tauri's custom protocol doesn't provide in sandboxed environments)
    viteSingleFile(),
  ],
  build: {
    // Target Safari 15 to ensure compatibility with WebKitGTK in Flatpak runtimes
    target: ["es2021", "safari15"],
  },

  // Pin dependency pre-bundling to the real app entry. By default Vite globs
  // `**/*.html` across the whole project to discover entries, which crawls
  // into `flatpak-build/` (var/run -> host /run, full of unrelated downloaded
  // HTML) and `src-tauri/target` codegen assets, then crashes the esbuild scan
  // with EPIPE. This is a single-page app, so one entry is all we need.
  optimizeDeps: {
    entries: ["index.html"],
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
