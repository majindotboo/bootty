import type { WebTerminalFrame } from "./terminal-types";

export type TerminalResize = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
  devicePixelRatio: number;
};

export type TerminalMouse = {
  kind: "move" | "down" | "up" | "leave" | "wheel";
  x: number;
  y: number;
  button: number;
};

export type TerminalKey = {
  kind: "down" | "up";
  key: string;
  code: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  repeat: boolean;
};

export type TerminalBackend = {
  label: string;
  start(): Promise<WebTerminalFrame>;
  readFrame(): Promise<WebTerminalFrame>;
  resize(request: TerminalResize): Promise<WebTerminalFrame>;
  write(input: string): Promise<void>;
  wantsKey?(event: TerminalKey, frame: WebTerminalFrame): boolean;
  key?(event: TerminalKey): Promise<WebTerminalFrame | null>;
  mouse?(event: TerminalMouse): Promise<WebTerminalFrame>;
  fps?(value: number): Promise<WebTerminalFrame>;
  selectedText?(): string | null | Promise<string | null>;
};
