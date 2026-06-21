import { selectedFrameText } from "./frame-utils";
import type { TerminalBackend, TerminalKey, TerminalMouse } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";
import { WebGlTerminalRenderer, type TerminalSelection } from "./webgl-terminal";

export type CanvasTerminalBackend = TerminalBackend | (() => TerminalBackend | Promise<TerminalBackend>);

export type MountCanvasTerminalOptions = {
  canvas: HTMLCanvasElement;
  backend: CanvasTerminalBackend;
  cols?: number;
  rows?: number;
  cellWidth?: number;
  cellHeight?: number;
  fps?: number;
  autoFocus?: boolean;
  signal?: AbortSignal;
  selection?: TerminalSelection | null;
  onFrame?: (frame: WebTerminalFrame) => void;
  onError?: (error: unknown) => void;
};

export type MountedCanvasTerminal = {
  readonly canvas: HTMLCanvasElement;
  readonly renderer: WebGlTerminalRenderer;
  readonly backend: TerminalBackend;
  readonly frame: WebTerminalFrame;
  refresh(): Promise<WebTerminalFrame>;
  resize(cols?: number, rows?: number): Promise<WebTerminalFrame>;
  write(input: string): Promise<void>;
  dispose(): void;
};

export async function mountCanvasTerminal(options: MountCanvasTerminalOptions): Promise<MountedCanvasTerminal> {
  const backend = typeof options.backend === "function" ? await options.backend() : options.backend;
  const renderer = new WebGlTerminalRenderer(options.canvas);
  const abortController = new AbortController();
  const signal = abortController.signal;
  const externalSignal = options.signal;
  let disposed = false;
  let refreshing = false;
  let frame = await backend.start();

  if (options.canvas.tabIndex < 0) {
    options.canvas.tabIndex = 0;
  }
  if (options.autoFocus ?? true) {
    options.canvas.focus();
  }

  async function render(nextFrame: WebTerminalFrame): Promise<WebTerminalFrame> {
    frame = nextFrame;
    renderer.render(frame, options.selection ?? null);
    options.onFrame?.(frame);
    return frame;
  }

  async function refresh(): Promise<WebTerminalFrame> {
    if (disposed || refreshing) {
      return frame;
    }
    refreshing = true;
    try {
      return await render(await backend.readFrame());
    } catch (error) {
      reportError(error, options.onError);
      return frame;
    } finally {
      refreshing = false;
    }
  }

  async function resize(cols?: number, rows?: number): Promise<WebTerminalFrame> {
    const nextGrid = gridForCanvas(options.canvas, frame, cols ?? options.cols, rows ?? options.rows);
    return render(
      await backend.resize({
        cols: nextGrid.cols,
        rows: nextGrid.rows,
        cellWidth: options.cellWidth ?? frame.cellWidth,
        cellHeight: options.cellHeight ?? frame.cellHeight,
        devicePixelRatio: window.devicePixelRatio || 1,
      }),
    );
  }

  async function write(input: string): Promise<void> {
    await backend.write(input);
    await refresh();
  }

  function dispose(): void {
    if (disposed) {
      return;
    }
    disposed = true;
    abortController.abort();
    renderer.dispose();
  }

  const report = (error: unknown) => reportError(error, options.onError);

  options.canvas.addEventListener("keydown", (event) => void handleKey(event, "down", backend, () => frame, refresh).catch(report), {
    signal,
  });
  options.canvas.addEventListener("keyup", (event) => void handleKey(event, "up", backend, () => frame, refresh).catch(report), {
    signal,
  });
  options.canvas.addEventListener("copy", (event) => void handleCopy(event, backend, () => frame).catch(report), { signal });
  options.canvas.addEventListener(
    "mousedown",
    (event) => void handleMouse(event, "down", backend, options.canvas, () => frame, refresh).catch(report),
    { signal },
  );
  options.canvas.addEventListener("mouseup", (event) => void handleMouse(event, "up", backend, options.canvas, () => frame, refresh).catch(report), {
    signal,
  });
  options.canvas.addEventListener(
    "mousemove",
    (event) => void handleMouse(event, "move", backend, options.canvas, () => frame, refresh).catch(report),
    { signal },
  );
  options.canvas.addEventListener(
    "mouseleave",
    (event) => void handleMouse(event, "leave", backend, options.canvas, () => frame, refresh).catch(report),
    { signal },
  );
  options.canvas.addEventListener("wheel", (event) => void handleMouse(event, "wheel", backend, options.canvas, () => frame, refresh).catch(report), {
    passive: false,
    signal,
  });

  const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => void resize().catch(report));
  resizeObserver?.observe(options.canvas.parentElement ?? options.canvas);
  signal.addEventListener("abort", () => resizeObserver?.disconnect(), { once: true });
  externalSignal?.addEventListener("abort", dispose, { once: true });

  const intervalMs = Math.max(16, Math.floor(1000 / (options.fps ?? 30)));
  const interval = window.setInterval(() => void refresh(), intervalMs);
  signal.addEventListener("abort", () => window.clearInterval(interval), { once: true });

  await resize(options.cols, options.rows);
  renderer.render(frame, options.selection ?? null);

  return {
    canvas: options.canvas,
    renderer,
    backend,
    get frame() {
      return frame;
    },
    refresh,
    resize,
    write,
    dispose,
  };
}

function gridForCanvas(
  canvas: HTMLCanvasElement,
  frame: WebTerminalFrame,
  cols: number | undefined,
  rows: number | undefined,
): { cols: number; rows: number } {
  if (cols && rows) {
    return { cols, rows };
  }
  const container = canvas.parentElement;
  const width = canvas.clientWidth || container?.clientWidth || window.innerWidth;
  const height = canvas.clientHeight || container?.clientHeight || window.innerHeight;
  return {
    cols: cols ?? Math.max(1, Math.ceil(width / frame.cellWidth)),
    rows: rows ?? Math.max(1, Math.ceil(height / frame.cellHeight)),
  };
}

async function handleKey(
  event: KeyboardEvent,
  kind: TerminalKey["kind"],
  backend: TerminalBackend,
  getFrame: () => WebTerminalFrame,
  refresh: () => Promise<WebTerminalFrame>,
): Promise<void> {
  if (kind === "down" && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c") {
    const text = await selectedText(backend, getFrame);
    if (text) {
      event.preventDefault();
      await writeClipboardText(text);
    }
    return;
  }

  const terminalKey = browserKey(event, kind);
  if (backend.wantsKey?.(terminalKey, getFrame()) && backend.key) {
    event.preventDefault();
    const frame = await backend.key(terminalKey);
    if (frame) {
      await refresh();
    }
    return;
  }

  if (kind !== "down") {
    return;
  }
  const input = encodeKey(event);
  if (!input) {
    return;
  }
  event.preventDefault();
  await backend.write(input);
  await refresh();
}

async function handleMouse(
  event: MouseEvent | WheelEvent,
  kind: TerminalMouse["kind"],
  backend: TerminalBackend,
  canvas: HTMLCanvasElement,
  getFrame: () => WebTerminalFrame,
  refresh: () => Promise<WebTerminalFrame>,
): Promise<void> {
  if (!backend.mouse) {
    return;
  }
  if (kind === "down") {
    canvas.focus({ preventScroll: true });
  }
  const button =
    kind === "wheel" ? Math.max(1, Math.min(8, Math.ceil(Math.abs((event as WheelEvent).deltaY) / 18))) * ((event as WheelEvent).deltaY < 0 ? -1 : 1) : kind === "down" ? Math.max(1, event.detail || 1) : "button" in event ? event.button : 0;
  if (kind === "wheel") {
    event.preventDefault();
  }
  if (kind !== "leave") {
    event.preventDefault();
  }
  const point = canvasCell(canvas, event.clientX, event.clientY, getFrame());
  await backend.mouse({
    kind,
    x: point.x,
    y: point.y,
    button,
  });
  await refresh();
}

async function handleCopy(event: ClipboardEvent, backend: TerminalBackend, getFrame: () => WebTerminalFrame): Promise<void> {
  const text = await selectedText(backend, getFrame);
  if (!text) {
    return;
  }
  event.preventDefault();
  event.clipboardData?.setData("text/plain", text);
}

async function selectedText(backend: TerminalBackend, getFrame: () => WebTerminalFrame): Promise<string | null> {
  return (await backend.selectedText?.()) ?? selectedFrameText(getFrame());
}

async function writeClipboardText(text: string): Promise<void> {
  try {
    await navigator.clipboard?.writeText(text);
    return;
  } catch {
    // Fall through to the execCommand path for browsers that reject async clipboard writes.
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.left = "-9999px";
  document.body.append(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

function browserKey(event: KeyboardEvent, kind: TerminalKey["kind"]): TerminalKey {
  return {
    kind,
    key: event.key,
    code: event.code,
    ctrlKey: event.ctrlKey,
    shiftKey: event.shiftKey,
    altKey: event.altKey,
    metaKey: event.metaKey,
    repeat: event.repeat,
  };
}

function canvasCell(canvas: HTMLCanvasElement, clientX: number, clientY: number, frame: WebTerminalFrame): { x: number; y: number } {
  const rect = canvas.getBoundingClientRect();
  const cssX = clientX - rect.left;
  const cssY = clientY - rect.top;
  return {
    x: Math.max(0, Math.min(frame.cols - 1, Math.floor((cssX / Math.max(1, rect.width)) * frame.cols))),
    y: Math.max(0, Math.min(frame.rows - 1, Math.floor((cssY / Math.max(1, rect.height)) * frame.rows))),
  };
}

function encodeKey(event: KeyboardEvent): string | null {
  if (event.metaKey) {
    return null;
  }
  if (event.ctrlKey && event.key.length === 1) {
    return String.fromCharCode(event.key.toUpperCase().charCodeAt(0) - 64);
  }
  switch (event.key) {
    case "Enter":
      return "\r";
    case "Backspace":
      return "\x7f";
    case "Tab":
      return "\t";
    case "Escape":
      return "\x1b";
    case "ArrowUp":
      return "\x1b[A";
    case "ArrowDown":
      return "\x1b[B";
    case "ArrowRight":
      return "\x1b[C";
    case "ArrowLeft":
      return "\x1b[D";
    case "PageUp":
      return "\x1b[5~";
    case "PageDown":
      return "\x1b[6~";
    case "Home":
      return "\x1b[H";
    case "End":
      return "\x1b[F";
    default:
      return event.key.length === 1 ? event.key : null;
  }
}

function reportError(error: unknown, onError: ((error: unknown) => void) | undefined): void {
  if (onError) {
    onError(error);
    return;
  }
  console.error(error);
}
