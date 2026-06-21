import { createRustSiteBackend, mountCanvasTerminal } from "bootty.js/browser";

const canvas = document.querySelector<HTMLCanvasElement>("#terminal");
if (!canvas) {
  throw new Error("Missing #terminal canvas");
}

await mountCanvasTerminal({
  canvas,
  backend: () => createRustSiteBackend(new URLSearchParams(window.location.search)),
  onError(error) {
    console.error(error);
  },
});
