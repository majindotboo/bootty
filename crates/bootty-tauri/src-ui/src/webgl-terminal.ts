import type { WebCell, WebColor, WebTerminalFrame } from "./terminal-types";

type Rgba = [number, number, number, number];

export class WebGlTerminalRenderer {
  private readonly gl: WebGL2RenderingContext;
  private readonly solidProgram: WebGLProgram;
  private readonly textProgram: WebGLProgram;
  private readonly solidBuffer: WebGLBuffer;
  private readonly textBuffer: WebGLBuffer;
  private readonly atlas: GlyphAtlas;
  private width = 0;
  private height = 0;
  private dpr = 1;

  constructor(private readonly canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
    });
    if (!gl) {
      throw new Error("WebGL2 is unavailable");
    }

    this.gl = gl;
    this.solidProgram = createProgram(gl, SOLID_VERTEX_SHADER, SOLID_FRAGMENT_SHADER);
    this.textProgram = createProgram(gl, TEXT_VERTEX_SHADER, TEXT_FRAGMENT_SHADER);
    this.solidBuffer = required(gl.createBuffer(), "create solid instance buffer");
    this.textBuffer = required(gl.createBuffer(), "create text instance buffer");
    this.atlas = new GlyphAtlas(gl);
  }

  render(frame: WebTerminalFrame): void {
    this.resize(frame);

    const solidInstances: number[] = [];
    const textInstances: number[] = [];
    pushSolidInstance(
      solidInstances,
      0,
      0,
      frame.cols * frame.cellWidth,
      frame.rows * frame.cellHeight,
      rgba(frame.colors.background),
    );

    for (const cell of frame.cells) {
      const colors = resolvedCellColors(frame, cell);
      if (colors.background) {
        pushSolidInstance(
          solidInstances,
          cell.x * frame.cellWidth,
          cell.y * frame.cellHeight,
          frame.cellWidth,
          frame.cellHeight,
          rgba(colors.background),
        );
      }
    }

    for (const cell of frame.cells) {
      if (cell.style.invisible || cell.text.length === 0) {
        continue;
      }

      const glyph = this.atlas.glyph(cell.text, frame.cellWidth, frame.cellHeight, this.dpr, cell.style);
      const color = resolvedCellColors(frame, cell).foreground;
      pushTextInstance(
        textInstances,
        cell.x * frame.cellWidth,
        cell.y * frame.cellHeight,
        glyph.width,
        glyph.height,
        glyph,
        rgba(color),
      );
      if (cell.style.underline) {
        pushSolidInstance(
          solidInstances,
          cell.x * frame.cellWidth,
          cell.y * frame.cellHeight + frame.cellHeight - 2,
          frame.cellWidth,
          1,
          rgba(color),
        );
      }
    }

    if (frame.cursor) {
      pushSolidInstance(
        solidInstances,
        frame.cursor.x * frame.cellWidth,
        frame.cursor.y * frame.cellHeight,
        frame.cellWidth,
        frame.cellHeight,
        rgba(frame.cursor.color ?? frame.colors.cursor ?? frame.colors.foreground, 0.8),
      );
    }

    this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    this.gl.disable(this.gl.DEPTH_TEST);
    this.gl.disable(this.gl.CULL_FACE);
    this.gl.clearColor(
      frame.colors.background.r / 255,
      frame.colors.background.g / 255,
      frame.colors.background.b / 255,
      1,
    );
    this.gl.clear(this.gl.COLOR_BUFFER_BIT);
    this.drawSolid(solidInstances);
    this.drawText(textInstances);
    this.gl.flush();
  }

  dispose(): void {
    this.gl.deleteBuffer(this.solidBuffer);
    this.gl.deleteBuffer(this.textBuffer);
    this.gl.deleteProgram(this.solidProgram);
    this.gl.deleteProgram(this.textProgram);
    this.atlas.dispose();
  }

  private resize(frame: WebTerminalFrame): void {
    this.dpr = window.devicePixelRatio || 1;
    this.width = Math.max(1, frame.cols * frame.cellWidth);
    this.height = Math.max(1, frame.rows * frame.cellHeight);
    const backingWidth = Math.ceil(this.width * this.dpr);
    const backingHeight = Math.ceil(this.height * this.dpr);
    if (this.canvas.width !== backingWidth || this.canvas.height !== backingHeight) {
      this.canvas.width = backingWidth;
      this.canvas.height = backingHeight;
    }
    this.canvas.style.width = `${this.width}px`;
    this.canvas.style.height = `${this.height}px`;
  }

  private drawSolid(instances: number[]): void {
    const count = instances.length / SOLID_INSTANCE_FLOATS;
    if (count === 0) {
      return;
    }

    const gl = this.gl;
    gl.useProgram(this.solidProgram);
    gl.uniform2f(
      required(gl.getUniformLocation(this.solidProgram, "u_resolution"), "solid resolution"),
      this.width,
      this.height,
    );
    gl.bindBuffer(gl.ARRAY_BUFFER, this.solidBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(instances), gl.STREAM_DRAW);
    bindInstancedAttribute(gl, this.solidProgram, "a_rect", 4, SOLID_INSTANCE_FLOATS, 0);
    bindInstancedAttribute(gl, this.solidProgram, "a_color", 4, SOLID_INSTANCE_FLOATS, 4);
    gl.disable(gl.BLEND);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
  }

  private drawText(instances: number[]): void {
    const count = instances.length / TEXT_INSTANCE_FLOATS;
    if (count === 0) {
      return;
    }

    const gl = this.gl;
    gl.useProgram(this.textProgram);
    gl.uniform2f(
      required(gl.getUniformLocation(this.textProgram, "u_resolution"), "text resolution"),
      this.width,
      this.height,
    );
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.atlas.texture);
    gl.uniform1i(required(gl.getUniformLocation(this.textProgram, "u_atlas"), "atlas sampler"), 0);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.textBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(instances), gl.STREAM_DRAW);
    bindInstancedAttribute(gl, this.textProgram, "a_rect", 4, TEXT_INSTANCE_FLOATS, 0);
    bindInstancedAttribute(gl, this.textProgram, "a_uv", 4, TEXT_INSTANCE_FLOATS, 4);
    bindInstancedAttribute(gl, this.textProgram, "a_color", 4, TEXT_INSTANCE_FLOATS, 8);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
    gl.disable(gl.BLEND);
  }
}

const SOLID_INSTANCE_FLOATS = 8;
const TEXT_INSTANCE_FLOATS = 12;

type Glyph = {
  u: number;
  v: number;
  w: number;
  h: number;
  width: number;
  height: number;
};

class GlyphAtlas {
  readonly texture: WebGLTexture;
  private readonly glyphs = new Map<string, Glyph>();
  private readonly tileCanvas = document.createElement("canvas");
  private readonly tileContext: CanvasRenderingContext2D;
  private next = 0;

  private static readonly size = 2048;
  private static readonly tileWidth = 96;
  private static readonly tileHeight = 48;

  constructor(private readonly gl: WebGL2RenderingContext) {
    this.texture = required(gl.createTexture(), "create glyph atlas texture");
    this.tileCanvas.width = GlyphAtlas.tileWidth;
    this.tileCanvas.height = GlyphAtlas.tileHeight;
    const context = this.tileCanvas.getContext("2d");
    if (!context) {
      throw new Error("2D canvas is unavailable for glyph atlas");
    }

    this.tileContext = context;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, GlyphAtlas.size, GlyphAtlas.size, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  glyph(text: string, cellWidth: number, cellHeight: number, dpr: number, style: WebCell["style"]): Glyph {
    const key = `${cellWidth}:${cellHeight}:${dpr}:${style.bold ? "b" : ""}${style.italic ? "i" : ""}:${text}`;
    const cached = this.glyphs.get(key);
    if (cached) {
      return cached;
    }

    const columns = Math.floor(GlyphAtlas.size / GlyphAtlas.tileWidth);
    const x = (this.next % columns) * GlyphAtlas.tileWidth;
    const y = Math.floor(this.next / columns) * GlyphAtlas.tileHeight;
    this.next += 1;
    if (y + GlyphAtlas.tileHeight > GlyphAtlas.size) {
      throw new Error("glyph atlas is full");
    }

    this.tileContext.setTransform(1, 0, 0, 1, 0, 0);
    this.tileContext.clearRect(0, 0, GlyphAtlas.tileWidth, GlyphAtlas.tileHeight);
    this.tileContext.fillStyle = "white";
    this.tileContext.textBaseline = "top";
    this.tileContext.textAlign = "left";
    this.tileContext.font = terminalFont(cellHeight * dpr, style);
    this.tileContext.fillText(text, 0, Math.round(2 * dpr));

    const metrics = this.tileContext.measureText(text);
    const glyphWidthPx = Math.min(GlyphAtlas.tileWidth, Math.max(Math.ceil(cellWidth * dpr), Math.ceil(metrics.width)));
    const glyphHeightPx = Math.min(GlyphAtlas.tileHeight, Math.max(Math.ceil(cellHeight * dpr), Math.ceil((cellHeight + 4) * dpr)));

    this.gl.bindTexture(this.gl.TEXTURE_2D, this.texture);
    this.gl.pixelStorei(this.gl.UNPACK_ALIGNMENT, 1);
    this.gl.texSubImage2D(this.gl.TEXTURE_2D, 0, x, y, this.gl.RGBA, this.gl.UNSIGNED_BYTE, this.tileCanvas);

    const glyph = {
      u: x / GlyphAtlas.size,
      v: y / GlyphAtlas.size,
      w: glyphWidthPx / GlyphAtlas.size,
      h: glyphHeightPx / GlyphAtlas.size,
      width: glyphWidthPx / dpr,
      height: glyphHeightPx / dpr,
    };
    this.glyphs.set(key, glyph);
    return glyph;
  }

  dispose(): void {
    this.gl.deleteTexture(this.texture);
  }
}

function pushSolidInstance(instances: number[], x: number, y: number, width: number, height: number, color: Rgba): void {
  instances.push(x, y, width, height, ...color);
}

function pushTextInstance(
  instances: number[],
  x: number,
  y: number,
  width: number,
  height: number,
  glyph: Glyph,
  color: Rgba,
): void {
  instances.push(x, y, width, height, glyph.u, glyph.v, glyph.w, glyph.h, ...color);
}

function bindInstancedAttribute(
  gl: WebGL2RenderingContext,
  program: WebGLProgram,
  name: string,
  size: number,
  strideFloats: number,
  offsetFloats: number,
): void {
  const location = gl.getAttribLocation(program, name);
  if (location < 0) {
    throw new Error(`missing WebGL attribute ${name}`);
  }
  gl.enableVertexAttribArray(location);
  gl.vertexAttribPointer(location, size, gl.FLOAT, false, strideFloats * 4, offsetFloats * 4);
  gl.vertexAttribDivisor(location, 1);
}

type ResolvedCellColors = {
  foreground: WebColor;
  background: WebColor | null;
};

function resolvedCellColors(frame: WebTerminalFrame, cell: WebCell): ResolvedCellColors {
  const foreground = cell.fg ?? frame.colors.foreground;
  const background = cell.bg;
  if (!cell.style.inverse) {
    return { foreground, background };
  }
  return {
    foreground: background ?? frame.colors.background,
    background: foreground,
  };
}

function terminalFont(cellHeight: number, style: WebCell["style"]): string {
  const slant = style.italic ? "italic " : "";
  const weight = style.bold ? "700 " : "";
  return `${slant}${weight}${Math.floor(cellHeight * 0.72)}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
}

function rgba(color: WebColor, alpha = 1): Rgba {
  return [color.r / 255, color.g / 255, color.b / 255, alpha];
}

function createProgram(gl: WebGL2RenderingContext, vertexSource: string, fragmentSource: string): WebGLProgram {
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = required(gl.createProgram(), "create WebGL program");
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error(gl.getProgramInfoLog(program) ?? "link WebGL program");
  }
  return program;
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = required(gl.createShader(type), "create WebGL shader");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(shader) ?? "compile WebGL shader");
  }
  return shader;
}

function required<T>(value: T | null, action: string): T {
  if (value == null) {
    throw new Error(`Failed to ${action}`);
  }
  return value;
}

const SOLID_VERTEX_SHADER = `#version 300 es
precision highp float;
in vec4 a_rect;
in vec4 a_color;
uniform vec2 u_resolution;
out vec4 v_color;

vec2 corner(uint index) {
  if (index == 0u) return vec2(0.0, 0.0);
  if (index == 1u) return vec2(1.0, 0.0);
  if (index == 2u) return vec2(0.0, 1.0);
  if (index == 3u) return vec2(0.0, 1.0);
  if (index == 4u) return vec2(1.0, 0.0);
  return vec2(1.0, 1.0);
}

void main() {
  vec2 position = a_rect.xy + corner(uint(gl_VertexID)) * a_rect.zw;
  vec2 clip = (position / u_resolution) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_color = a_color;
}`;

const SOLID_FRAGMENT_SHADER = `#version 300 es
precision highp float;
in vec4 v_color;
out vec4 out_color;

void main() {
  out_color = v_color;
}`;

const TEXT_VERTEX_SHADER = `#version 300 es
precision highp float;
in vec4 a_rect;
in vec4 a_uv;
in vec4 a_color;
uniform vec2 u_resolution;
out vec2 v_uv;
out vec4 v_color;

vec2 corner(uint index) {
  if (index == 0u) return vec2(0.0, 0.0);
  if (index == 1u) return vec2(1.0, 0.0);
  if (index == 2u) return vec2(0.0, 1.0);
  if (index == 3u) return vec2(0.0, 1.0);
  if (index == 4u) return vec2(1.0, 0.0);
  return vec2(1.0, 1.0);
}

void main() {
  vec2 local = corner(uint(gl_VertexID));
  vec2 position = a_rect.xy + local * a_rect.zw;
  vec2 clip = (position / u_resolution) * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_uv = a_uv.xy + local * a_uv.zw;
  v_color = a_color;
}`;

const TEXT_FRAGMENT_SHADER = `#version 300 es
precision highp float;
uniform sampler2D u_atlas;
in vec2 v_uv;
in vec4 v_color;
out vec4 out_color;

void main() {
  float alpha = texture(u_atlas, v_uv).a;
  out_color = vec4(v_color.rgb, v_color.a * alpha);
}`;