export { cellAt, createBlankCell, createEmptyFrame, frameRows, frameSize, frameToText, type TerminalFrameSize } from "./frame-utils";
export type { TerminalBackend, TerminalKey, TerminalMouse, TerminalResize } from "./terminal-api";
export type {
  WebCell,
  WebColor,
  WebEguiFrame,
  WebEguiLabel,
  WebEguiLink,
  WebEguiMesh,
  WebEguiTexture,
  WebImage,
  WebImageLayer,
  WebRect,
  WebTerminalFrame,
} from "./terminal-types";

const BROWSER_ONLY_MESSAGE =
  "bootty.js WebGL rendering requires a browser runtime. Import bootty.js/browser in browser bundles, or use bootty.js/node for frame/text utilities only.";

export class WebGlTerminalRenderer {
  constructor(..._args: unknown[]) {
    throw new Error(BROWSER_ONLY_MESSAGE);
  }

  render(): never {
    throw new Error(BROWSER_ONLY_MESSAGE);
  }

  dispose(): never {
    throw new Error(BROWSER_ONLY_MESSAGE);
  }
}
