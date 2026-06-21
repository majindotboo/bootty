import { cp, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = resolve(packageRoot, "src/site-wasm");
const distWasmDir = resolve(packageRoot, "dist/site-wasm");
const bundledWasmTarget = resolve(packageRoot, "dist/bootty_site_bg.wasm");

await mkdir(distWasmDir, { recursive: true });
await cp(source, distWasmDir, { recursive: true, force: true });
await cp(resolve(source, "bootty_site_bg.wasm"), bundledWasmTarget, { force: true });
