import type { WebCell, WebColor, WebTerminalFrame } from "./terminal-types";

export type TerminalFrameSize = {
  cols: number;
  rows: number;
  width: number;
  height: number;
  cellWidth: number;
  cellHeight: number;
};

export function frameSize(frame: WebTerminalFrame): TerminalFrameSize {
  return {
    cols: frame.cols,
    rows: frame.rows,
    width: frame.cols * frame.cellWidth,
    height: frame.rows * frame.cellHeight,
    cellWidth: frame.cellWidth,
    cellHeight: frame.cellHeight,
  };
}

export function cellAt(frame: WebTerminalFrame, x: number, y: number): WebCell | undefined {
  return frame.cells.find((cell) => cell.x === x && cell.y === y);
}

export function frameRows(frame: WebTerminalFrame, options: { trimRight?: boolean } = {}): string[] {
  const trimRight = options.trimRight ?? true;
  const cells = new Map(frame.cells.map((cell) => [`${cell.x}:${cell.y}`, cell]));
  const rows: string[] = [];

  for (let y = 0; y < frame.rows; y += 1) {
    let row = "";
    for (let x = 0; x < frame.cols; x += 1) {
      const cell = cells.get(`${x}:${y}`);
      row += cell && !cell.style.invisible ? cell.text || " " : " ";
    }
    rows.push(trimRight ? row.replace(/\s+$/u, "") : row);
  }

  return rows;
}

export function frameToText(frame: WebTerminalFrame, options: { trimRight?: boolean } = {}): string {
  return frameRows(frame, options).join("\n");
}

export function selectedFrameText(frame: WebTerminalFrame): string | null {
  const selection = frame.selection;
  if (!selection) {
    return null;
  }
  const [start, end] =
    selection.anchor.y < selection.focus.y ||
    (selection.anchor.y === selection.focus.y && selection.anchor.x <= selection.focus.x)
      ? [selection.anchor, selection.focus]
      : [selection.focus, selection.anchor];
  const cells = new Map(frame.cells.map((cell) => [`${cell.x}:${cell.y}`, cell]));
  const rows: string[] = [];
  for (let y = start.y; y <= end.y; y += 1) {
    const from = y === start.y ? start.x : 0;
    const to = y === end.y ? end.x : frame.cols - 1;
    let row = "";
    for (let x = from; x <= to; x += 1) {
      const cell = cells.get(`${x}:${y}`);
      row += cell && !cell.style.invisible ? cell.text || " " : " ";
    }
    rows.push(row.replace(/\s+$/u, ""));
  }
  const text = rows.join("\n").trimEnd();
  return text.length > 0 ? text : null;
}

export function createBlankCell(x: number, y: number, overrides: Partial<WebCell> = {}): WebCell {
  return {
    x,
    y,
    text: " ",
    fg: null,
    bg: null,
    osc8: null,
    style: {
      bold: false,
      italic: false,
      faint: false,
      blink: false,
      inverse: false,
      invisible: false,
      strikethrough: false,
      overline: false,
      underline: false,
    },
    ...overrides,
  };
}

export function createEmptyFrame(options: {
  cols: number;
  rows: number;
  cellWidth?: number;
  cellHeight?: number;
  foreground?: WebColor;
  background?: WebColor;
}): WebTerminalFrame {
  const foreground = options.foreground ?? { r: 205, g: 214, b: 244 };
  const background = options.background ?? { r: 17, g: 17, b: 27 };
  return {
    cols: options.cols,
    rows: options.rows,
    cellWidth: options.cellWidth ?? 10,
    cellHeight: options.cellHeight ?? 18,
    colors: {
      foreground,
      background,
      cursor: foreground,
    },
    cursor: null,
    cells: [],
    images: [],
    egui: null,
  };
}
