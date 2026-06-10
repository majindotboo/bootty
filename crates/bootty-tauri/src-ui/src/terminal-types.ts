export type WebColor = { r: number; g: number; b: number };

export type WebCell = {
  x: number;
  y: number;
  text: string;
  fg: WebColor | null;
  bg: WebColor | null;
  osc8: string | null;
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
export type WebRect = {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
};

export type WebImageLayer = "belowBackground" | "belowText" | "aboveText";

export type WebImage = {
  key: string;
  layer: WebImageLayer;
  imageWidth: number;
  imageHeight: number;
  source: WebRect;
  destination: WebRect;
  pixelFormat?: "rgba" | "bgrx";
  revision?: number;
  rgba: ArrayLike<number>;
};

export type WebEguiTexture = {
  id: string;
  width: number;
  height: number;
  rgba: ArrayLike<number>;
};

export type WebEguiMesh = {
  textureId: string;
  clip: WebRect;
  vertices: number[];
  indices: number[];
};

export type WebEguiLabel = {
  x: number;
  y: number;
  text: string;
  size: number;
  color: WebColor;
  align: "left" | "center" | "right";
};

export type WebEguiLink = {
  rect: WebRect;
  url: string;
};

export type WebEguiFrame = {
  textures: WebEguiTexture[];
  meshes: WebEguiMesh[];
  labels: WebEguiLabel[];
  links: WebEguiLink[];
};

export type WebTerminalFrame = {
  selected?: number;
  focus?: "menu" | "detail";
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
  images: WebImage[];
  egui?: WebEguiFrame | null;
};
