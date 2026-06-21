import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const boottyBrowserEntry = fileURLToPath(new URL("../../packages/bootty-js/src/browser.ts", import.meta.url));

export default defineConfig({
  base: "./",
  resolve: {
    alias: {
      "bootty.js/browser": boottyBrowserEntry,
    },
  },
  build: {
    outDir: "../../pages-dist",
    emptyOutDir: true,
  },
});
