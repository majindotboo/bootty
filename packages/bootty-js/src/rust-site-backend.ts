import initSiteWasm, { SiteBackend, site_navigation } from "./site-wasm/bootty_site";
import type { TerminalBackend, TerminalMouse, TerminalResize } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";

let wasmReady: Promise<unknown> | null = null;

export type RustSiteBackendOptions = URLSearchParams | { page?: string | null } | string | null | undefined;

export type RustSiteNavigationItem = {
  slug: string;
  label: string;
  path: string;
};

export async function rustSiteNavigation(): Promise<RustSiteNavigationItem[]> {
  if (!wasmReady) {
    wasmReady = initSiteWasm();
  }
  await wasmReady;
  return site_navigation() as RustSiteNavigationItem[];
}

export async function createRustSiteBackend(options?: RustSiteBackendOptions): Promise<TerminalBackend> {
  if (!wasmReady) {
    wasmReady = initSiteWasm();
  }
  await wasmReady;

  const page = pageFromOptions(options);
  const site = page ? SiteBackend.for_page(page) : SiteBackend.new();
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
    selectedText() {
      return site.selected_text() ?? null;
    },
  };
}

function pageFromOptions(options: RustSiteBackendOptions): string | null {
  if (!options) {
    return null;
  }
  if (typeof options === "string") {
    return options;
  }
  if (options instanceof URLSearchParams) {
    return options.get("page");
  }
  return options.page ?? null;
}
