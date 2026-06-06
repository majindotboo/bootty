import type { WebTerminalFrame } from "./terminal-types";

export type TerminalResize = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
};

export type TerminalBackend = {
  label: string;
  start(): Promise<WebTerminalFrame>;
  readFrame(): Promise<WebTerminalFrame>;
  resize(request: TerminalResize): Promise<WebTerminalFrame>;
  write(input: string): Promise<void>;
};

export async function createTerminalBackend(): Promise<TerminalBackend> {
  if (import.meta.env.MODE === "github-pages" || import.meta.env.VITE_TERMINAL_BACKEND === "fake") {
    const { createFakeShellBackend } = await import("./fake-shell-backend");
    return createFakeShellBackend();
  }

  const { createTauriTerminalBackend } = await import("./tauri-terminal-backend");
  return createTauriTerminalBackend();
}