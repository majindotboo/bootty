import initSiteWasm, { SiteBackend } from "./bootty-site-wasm/bootty_site";
import type { TerminalBackend, TerminalMouse, TerminalResize } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";

let wasmReady: Promise<unknown> | null = null;
export async function createRustSiteBackend(_search = new URLSearchParams()): Promise<TerminalBackend> {
  if (!wasmReady) {
    wasmReady = initSiteWasm();
  }
  await wasmReady;

  const site = SiteBackend.new();
  let lastFrame = site.frame() as WebTerminalFrame;

  return {
    label: "bootty site",
    async start() {
      lastFrame = site.frame() as WebTerminalFrame;
      return lastFrame;
    },
    async readFrame() {
      lastFrame = site.frame() as WebTerminalFrame;
      return lastFrame;
    },
    async resize(request: TerminalResize) {
      lastFrame = site.resize(request.cols, request.rows, request.devicePixelRatio) as WebTerminalFrame;
      return lastFrame;
    },
    async write(input: string) {
      lastFrame = site.input(input) as WebTerminalFrame;
    },
    async mouse(event: TerminalMouse) {
      lastFrame = site.mouse(event.kind, event.x, event.y, event.button) as WebTerminalFrame;
      return lastFrame;
    },
    async fps(value: number) {
      lastFrame = site.set_fps(value) as WebTerminalFrame;
      return lastFrame;
    },
  };
}

