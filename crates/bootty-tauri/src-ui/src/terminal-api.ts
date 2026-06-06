import { invoke } from "@tauri-apps/api/core";
import type { WebTerminalFrame } from "./terminal-types";

export type TerminalResize = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
};

export function startTerminal(): Promise<WebTerminalFrame> {
  return invoke<WebTerminalFrame>("start_terminal");
}

export function readTerminalFrame(): Promise<WebTerminalFrame> {
  return invoke<WebTerminalFrame>("terminal_frame");
}

export function resizeTerminal(request: TerminalResize): Promise<WebTerminalFrame> {
  return invoke<WebTerminalFrame>("resize_terminal", { request });
}

export function writeTerminal(input: string): Promise<void> {
  return invoke("write_terminal", { input });
}