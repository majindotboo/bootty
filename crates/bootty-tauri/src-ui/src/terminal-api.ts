import type { WebTerminalFrame } from "./terminal-types";

export type TerminalResize = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
};

export type TerminalMouse = {
  kind: "move" | "down" | "up" | "leave";
  x: number;
  y: number;
  button: number;
};

export type TerminalBackend = {
  label: string;
  start(): Promise<WebTerminalFrame>;
  readFrame(): Promise<WebTerminalFrame>;
  resize(request: TerminalResize): Promise<WebTerminalFrame>;
  write(input: string): Promise<void>;
  mouse?(event: TerminalMouse): Promise<WebTerminalFrame>;
  fps?(value: number): Promise<WebTerminalFrame>;
};

export async function createTerminalBackend(): Promise<TerminalBackend> {
  if (import.meta.env.MODE === "github-pages" || import.meta.env.VITE_TERMINAL_BACKEND === "site") {
    const { createRustSiteBackend } = await import("./rust-site-backend");
    return createRustSiteBackend();
  }

  const { createTauriTerminalBackend } = await import("./tauri-terminal-backend");
  return createTauriTerminalBackend();
}