import { invoke } from "@tauri-apps/api/core";
import type { TerminalBackend, TerminalResize } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";

export function createTauriTerminalBackend(): TerminalBackend {
  return {
    label: "tauri pty backend",
    start: () => invoke<WebTerminalFrame>("start_terminal"),
    readFrame: () => invoke<WebTerminalFrame>("terminal_frame"),
    resize: (request: TerminalResize) => invoke<WebTerminalFrame>("resize_terminal", { request }),
    write: (input: string) => invoke("write_terminal", { input }),
  };
}