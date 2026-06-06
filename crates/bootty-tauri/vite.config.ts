import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST ?? "127.0.0.1";

export default defineConfig({
  plugins: [react()],
  root: "src-ui",
  clearScreen: false,
  server: {
    host,
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
});