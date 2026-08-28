import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// The dashboard is served from the AxVisor host root (`/web/axvisor-ui/current`)
// by `tower-http::ServeDir`, so assets are emitted with absolute `/assets/...`
// paths and the entry HTML references them directly. `base: "/"` keeps those
// absolute paths (no CDN, no relative-path rewriting).
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    outDir: "dist",
    sourcemap: false,
    // Deterministic output: stable module/named-chunk ids and no timestamps.
    rollupOptions: {
      output: {
        entryFileNames: "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash].[ext]",
      },
    },
  },
});
