export type WebColor = { r: number; g: number; b: number };

export type WebCell = {
  x: number;
  y: number;
  text: string;
  fg: WebColor | null;
  bg: WebColor | null;
  style: {
    bold: boolean;
    italic: boolean;
    faint: boolean;
    blink: boolean;
    inverse: boolean;
    invisible: boolean;
    strikethrough: boolean;
    overline: boolean;
    underline: boolean;
  };
};

export type WebTerminalFrame = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
  colors: {
    background: WebColor;
    foreground: WebColor;
    cursor: WebColor | null;
  };
  cursor: { x: number; y: number; color: WebColor | null } | null;
  cells: WebCell[];
};