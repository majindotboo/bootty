import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST ?? "127.0.0.1";

export default defineConfig(({ mode }) => {
  const githubPages = mode === "github-pages";

  return {
    plugins: [react()],
    root: "src-ui",
    envDir: ".",
    base: githubPages ? "./" : "/",
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
      outDir: githubPages ? "../pages-dist" : "../dist",
      emptyOutDir: true,
    },
  };
});